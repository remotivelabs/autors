//! ISO-TP (ISO 15765-2) transport layer: message states and a pure byte-level
//! segmentation/reassembly state machine.
//! The core types are [`IsoTpFsm`] (send/receive state machine over raw frame
//! bytes), [`MsgState`] (transfer status), and the frame type / flow control
//! enums. CAN-level transmit/receive, flow-control waiting, and timeout/abort
//! handling live in [`crate::transport`]; [`IsoTpFsm::on_received_can`] accepts
//! whole CAN frames.

use autors_can::frame::CanFrame;

use crate::error::{Error, Result};

/// Numeric-enum helper macro (mirrors the ones in autors-diag doip.rs/kwp.rs).
macro_rules! num_enum {
    ($(#[$meta:meta])* $vis:vis $name:ident : $t:ty { $( $(#[$vmeta:meta])* $var:ident = $val:expr ),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr($t)]
        $vis enum $name {
            $( $(#[$vmeta])* $var = $val ),+
        }
        impl $name {
            /// Numeric value to enum; unknown values return `None`.
            pub fn from_num(v: $t) -> Option<Self> {
                match v {
                    $( x if x == Self::$var as $t => Some(Self::$var), )+
                    _ => None,
                }
            }
            /// Enum to numeric value.
            pub fn to_num(self) -> $t {
                self as $t
            }
        }
    };
}

num_enum! {
    /// ISO 15765-2 flow control status.
    pub FlowStatus : u8 {
        /// Continue sending (CTS).
        ClearToSend = 0,
        /// Wait.
        Wait = 1,
        /// Overflow.
        Overflow = 2,
    }
}

num_enum! {
    /// ISO 15765-2 frame type (high nibble of the PCI byte).
    pub FrameType : u8 {
        /// Single frame.
        SF = 0x00,
        /// First frame.
        FF = 0x10,
        /// Consecutive frame.
        CF = 0x20,
        /// Flow control frame.
        FC = 0x30,
    }
}

num_enum! {
    /// Message transfer state machine status.
    /// The discriminants are signed and ordered: pending states < `Success`(0) <
    /// error states, so comparisons like `state < MsgState::Success` work as
    /// expected (`Ord` is derived for exactly this purpose).
    #[derive(PartialOrd, Ord)]
    pub MsgState : i32 {
        /// Waiting to send (data frame).
        PendingSnd = -4,
        /// Waiting to send a flow control frame.
        PendingSndFC = -3,
        /// Waiting to receive a flow control frame.
        PendingRcvFC = -2,
        /// Waiting to receive.
        PendingRcv = -1,
        /// Completed successfully.
        Success = 0,
        /// Request message length exceeded the limit.
        ErrRequestLenExceeded = 1,
        /// Another message is already being processed.
        ErrMsgInProcess = 2,
        /// Failed to send a request frame.
        ErrSendRequest = 3,
        /// Unexpected sequence number.
        ErrUnexpectedSequenceNo = 4,
        /// Timeout awaiting a consecutive frame.
        ErrTimeoutAwaitingCFFrame = 5,
        /// Received an unexpected flow control frame.
        ErrReceivedUnexpectedFCFrame = 6,
        /// The peer reported a flow control overflow.
        ErrFlowControlOverflow = 7,
        /// Global request timeout.
        ErrTimeout = 8,
        /// Response service ID mismatch.
        ErrUnexpectedRSID = 9,
        /// Generic error.
        ErrGeneric = 10,
    }
}

impl MsgState {
    /// Human-readable description text for each variant; variants without a
    /// dedicated text return their variant name.
    pub fn description(self) -> &'static str {
        match self {
            MsgState::PendingSnd => "PendingSnd",
            MsgState::PendingSndFC => "PendingSndFC",
            MsgState::PendingRcvFC => "PendingRcvFC",
            MsgState::PendingRcv => "PendingRcv",
            MsgState::Success => "Success",
            // The message text below is part of the behavioral contract
            // (including the original spelling "exeeded").
            MsgState::ErrRequestLenExceeded => "Message request length exeeded",
            MsgState::ErrMsgInProcess => "There is already a message to process",
            MsgState::ErrSendRequest => "Failed to send a request frame",
            MsgState::ErrUnexpectedSequenceNo => "unexpected sequence number",
            MsgState::ErrTimeoutAwaitingCFFrame => "Timeout awaiting consecutive frame",
            MsgState::ErrReceivedUnexpectedFCFrame => "ErrReceivedUnexpectedFCFrame",
            MsgState::ErrFlowControlOverflow => "Server reported a flow control overflow",
            MsgState::ErrTimeout => "Global request timeout",
            MsgState::ErrUnexpectedRSID => "Unexpected response service id",
            MsgState::ErrGeneric => "Generic error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsoTpType {
    Can,
    CanFd,
    /// ISO 17987-2 diagnostic transport over LIN.
    Lin,
    /// FlexRay.
    FlexRay,
}

pub const NO_RESPONSE_MASK: u8 = 0x80;

pub const MAX_MSG_LEN_CAN: u32 = 4095;
pub const MAX_MSG_LEN_CANFD: u32 = u32::MAX;
/// Maximum ISO 17987-2 LIN transport message length.
pub const MAX_MSG_LEN_LIN: u32 = 4095;

#[derive(Debug, Clone)]
pub struct IsoTpFsm {
    await_response: bool,
    total_len: usize,
    seq_no: u8,
    block_count: i32,
    frame_size: usize,
    fc_block_size: u8,
    pub request_st_min: i32,
    st_min: u8,
    block_size: u8,
    frame_type: FrameType,
    send_data: Vec<u8>,
    recv_data: Vec<u8>,
    state: MsgState,
    send_offset: usize,
}

impl IsoTpFsm {
    pub fn new(
        data_to_send: Vec<u8>,
        await_response: bool,
        frame_size: u8,
        block_size: u8,
        st_min: u8,
    ) -> Self {
        let fs = frame_size as usize;
        let sf_capacity = if fs <= 8 { fs - 1 } else { fs - 2 };
        IsoTpFsm {
            await_response,
            total_len: 0,
            seq_no: 0,
            block_count: 0,
            frame_size: fs,
            fc_block_size: 0,
            request_st_min: 0,
            st_min,
            block_size,
            frame_type: if data_to_send.len() > sf_capacity {
                FrameType::FF
            } else {
                FrameType::SF
            },
            send_data: data_to_send,
            recv_data: Vec::new(),
            state: MsgState::PendingSnd,
            send_offset: 0,
        }
    }

    pub fn frame_type(&self) -> FrameType {
        self.frame_type
    }

    pub fn state(&self) -> MsgState {
        self.state
    }

    pub fn received(&self) -> &[u8] {
        &self.recv_data
    }

    pub fn take_received(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.recv_data)
    }

    pub fn send_offset(&self) -> usize {
        self.send_offset
    }

    fn st_min_to_micro_secs(v: u8) -> i32 {
        if v <= 127 {
            1000 * i32::from(v)
        } else if (241..=249).contains(&v) {
            i32::from(v & 0x0F) * 100
        } else {
            0
        }
    }

    pub fn next_frame(&mut self) -> (MsgState, Option<Vec<u8>>) {
        let mut out: Option<Vec<u8>> = None;
        let mut data_len = 0usize;
        match self.state {
            MsgState::PendingSndFC => {
                out = Some(vec![FrameType::FC.to_num(), self.block_size, self.st_min]);
                self.state = MsgState::PendingRcv;
            }
            MsgState::PendingSnd => match self.frame_type {
                FrameType::SF => {
                    data_len = self.send_data.len();
                    let mut f = Vec::with_capacity(2 + data_len);
                    if self.send_data.len() > 7 {
                        f.push(0);
                        f.push(self.send_data.len() as u8);
                    } else {
                        f.push(self.send_data.len() as u8 & 0x0F);
                    }
                    out = Some(f);
                    self.state = if self.await_response {
                        MsgState::PendingRcv
                    } else {
                        MsgState::Success
                    };
                }
                FrameType::FF => {
                    let count = self.send_data.len();
                    let mut f = Vec::with_capacity(self.frame_size);
                    if count > 4095 {
                        f.push(0x10);
                        f.push(0);
                        f.extend_from_slice(&(count as u32).to_be_bytes());
                        data_len = self.frame_size - 6;
                    } else {
                        f.push(0x10 | ((count >> 8) as u8 & 0x0F));
                        f.push(count as u8);
                        data_len = self.frame_size - 2;
                    }
                    out = Some(f);
                    self.state = MsgState::PendingRcvFC;
                    self.frame_type = FrameType::CF;
                    self.seq_no = self.seq_no.wrapping_add(1);
                }
                FrameType::CF => {
                    data_len = (self.send_data.len() - self.send_offset).min(self.frame_size - 1);
                    out = Some(vec![0x20 | (self.seq_no % 16)]);
                    self.seq_no = self.seq_no.wrapping_add(1);
                    if self.send_data.len() == self.send_offset + data_len {
                        self.state = MsgState::PendingRcv;
                    } else {
                        self.block_count -= 1;
                        if self.block_count == 0 && self.fc_block_size > 0 {
                            self.block_count = i32::from(self.fc_block_size);
                            self.state = MsgState::PendingRcvFC;
                        }
                    }
                }
                FrameType::FC => {}
            },
            _ => {}
        }
        let out = out.map(|mut f| {
            if data_len > 0 {
                f.extend_from_slice(&self.send_data[self.send_offset..self.send_offset + data_len]);
                self.send_offset += data_len;
            }
            f
        });
        (self.state, out)
    }

    pub fn on_received(&mut self, data: &[u8]) -> Result<MsgState> {
        let &first = data
            .first()
            .ok_or_else(|| Error::Protocol("ISO-TP: empty frame".to_string()))?;
        let mut offset = 0usize;
        let mut data_len = 0usize;
        match first & 0xF0 {
            0x00 => {
                data_len = usize::from(first & 0x0F);
                offset = 1;
                if data_len == 0 {
                    let &len = data.get(1).ok_or_else(|| {
                        Error::Protocol("ISO-TP: short SF escape frame".to_string())
                    })?;
                    data_len = usize::from(len);
                    offset = 2;
                }
                if data.get(1) == Some(&0x7F) && data.get(3) == Some(&0x78) {
                    data_len = 0;
                } else {
                    self.state = MsgState::Success;
                }
            }
            0x10 => {
                let &b1 = data
                    .get(1)
                    .ok_or_else(|| Error::Protocol("ISO-TP: short FF frame".to_string()))?;
                self.seq_no = 0;
                self.total_len = (usize::from(first & 0x0F) << 8) + usize::from(b1);
                offset = 2;
                if self.total_len == 0 {
                    if data.len() < 6 {
                        return Err(Error::Protocol("ISO-TP: short FF escape frame".to_string()));
                    }
                    self.total_len =
                        u32::from_be_bytes([data[2], data[3], data[4], data[5]]) as usize;
                    offset = 6;
                }
                data_len = data.len() - offset;
                self.state = MsgState::PendingSndFC;
                self.block_count = i32::from(self.block_size);
            }
            0x20 => {
                self.seq_no = self.seq_no.wrapping_add(1);
                let sn = first & 0x0F;
                offset = 1;
                if self.seq_no % 16 != sn {
                    self.state = MsgState::ErrUnexpectedSequenceNo;
                } else {
                    data_len = data.len() - offset;
                    if self.total_len <= self.recv_data.len() + data_len {
                        self.state = MsgState::Success;
                        data_len = self.total_len.saturating_sub(self.recv_data.len());
                    } else if self.block_size > 0 {
                        self.block_count -= 1;
                        if self.block_count == 0 {
                            self.state = MsgState::PendingSndFC;
                            self.block_count = i32::from(self.block_size);
                        }
                    }
                }
            }
            0x30 => {
                if self.state != MsgState::PendingRcvFC {
                    self.state = MsgState::ErrReceivedUnexpectedFCFrame;
                } else {
                    if data.len() < 3 {
                        return Err(Error::Protocol("ISO-TP: short FC frame".to_string()));
                    }
                    match first & 0x0F {
                        x if x == FlowStatus::Overflow.to_num() => {
                            self.state = MsgState::ErrFlowControlOverflow;
                        }
                        x if x == FlowStatus::ClearToSend.to_num() => {
                            self.state = MsgState::PendingSnd;
                        }
                        _ => {}
                    }
                    self.fc_block_size = data[1];
                    self.block_count = i32::from(self.fc_block_size);
                    self.request_st_min = Self::st_min_to_micro_secs(data[2]);
                    offset = 3;
                }
            }
            _ => {}
        }
        if data_len > 0 {
            let end = (offset + data_len).min(data.len());
            self.recv_data.extend_from_slice(&data[offset..end]);
        }
        Ok(self.state)
    }

    pub fn on_received_fr(&mut self, data: &[u8]) -> Result<MsgState> {
        if data.len() < 4 {
            return Err(Error::Protocol("ISO-TP: short FlexRay frame".to_string()));
        }
        let payload = &data[4..];
        let _addr1 = u16::from_be_bytes([payload[0], payload[1]]);
        let _addr2 = u16::from_be_bytes([payload[2], payload[3]]);
        self.on_received(payload)
    }

    pub fn on_received_can(&mut self, frame: &CanFrame) -> Result<MsgState> {
        self.on_received(&frame.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_state_values_and_ordering() {
        assert_eq!(MsgState::PendingSnd.to_num(), -4);
        assert_eq!(MsgState::PendingRcv.to_num(), -1);
        assert_eq!(MsgState::Success.to_num(), 0);
        assert_eq!(MsgState::ErrGeneric.to_num(), 10);
        assert!(MsgState::PendingSnd < MsgState::Success);
        assert!(MsgState::PendingRcvFC < MsgState::Success);
        assert!(MsgState::ErrTimeout > MsgState::Success);
        assert_eq!(MsgState::from_num(-3), Some(MsgState::PendingSndFC));
        assert_eq!(MsgState::from_num(99), None);
    }

    #[test]
    fn msg_state_descriptions() {
        assert_eq!(
            MsgState::ErrRequestLenExceeded.description(),
            "Message request length exeeded"
        );
        assert_eq!(MsgState::ErrTimeout.description(), "Global request timeout");
        assert_eq!(MsgState::Success.description(), "Success");
        assert_eq!(
            MsgState::ErrFlowControlOverflow.description(),
            "Server reported a flow control overflow"
        );
    }

    #[test]
    fn send_single_frame() {
        let mut fsm = IsoTpFsm::new(vec![0x22, 0xF1, 0x90], true, 8, 0, 0);
        assert_eq!(fsm.frame_type(), FrameType::SF);
        let (state, frame) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingRcv);
        assert_eq!(frame.unwrap(), vec![0x03, 0x22, 0xF1, 0x90]);
    }

    #[test]
    fn send_single_frame_no_response() {
        let mut fsm = IsoTpFsm::new(vec![0x22, 0xF1, 0x90], false, 8, 0, 0);
        let (state, _) = fsm.next_frame();
        assert_eq!(state, MsgState::Success);
    }

    #[test]
    fn send_multi_frame_with_fc() {
        let data: Vec<u8> = (1..=20).collect();
        let mut fsm = IsoTpFsm::new(data, true, 8, 0, 0);
        assert_eq!(fsm.frame_type(), FrameType::FF);
        let (state, frame) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingRcvFC);
        assert_eq!(
            frame.unwrap(),
            vec![0x10, 0x14, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06]
        );
        let state = fsm.on_received(&[0x30, 0x00, 0x00]).unwrap();
        assert_eq!(state, MsgState::PendingSnd);
        // CF1
        let (state, frame) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingSnd);
        assert_eq!(
            frame.unwrap(),
            vec![0x21, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D]
        );
        let (state, frame) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingRcv);
        assert_eq!(
            frame.unwrap(),
            vec![0x22, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14]
        );
        let (_, frame) = fsm.next_frame();
        assert!(frame.is_none());
    }

    #[test]
    fn send_block_end_triggers_pending_rcv_fc() {
        let data: Vec<u8> = (1..=30).collect();
        let mut fsm = IsoTpFsm::new(data, true, 8, 0, 0);
        let _ = fsm.next_frame(); // FF
        let state = fsm.on_received(&[0x30, 0x02, 0xF3]).unwrap();
        assert_eq!(state, MsgState::PendingSnd);
        assert_eq!(fsm.request_st_min, 300);
        let (state, _) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingSnd);
        let (state, _) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingRcvFC);
    }

    #[test]
    fn receive_single_frame() {
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 8, 0, 0);
        let state = fsm.on_received(&[0x03, 0x22, 0xF1, 0x90]).unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(fsm.received(), &[0x22, 0xF1, 0x90]);
    }

    #[test]
    fn receive_negative_response_pending_is_not_final() {
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 8, 0, 0);
        let state = fsm.on_received(&[0x03, 0x7F, 0x22, 0x78]).unwrap();
        assert_eq!(state, MsgState::PendingSnd);
        let state = fsm.on_received(&[0x03, 0x7F, 0x22, 0x31]).unwrap();
        assert_eq!(state, MsgState::Success);
    }

    #[test]
    fn receive_multi_frame() {
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 8, 0, 0);
        let state = fsm.on_received(&[0x10, 0x0A, 1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(state, MsgState::PendingSndFC);
        let (state, fc) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingRcv);
        assert_eq!(fc.unwrap(), vec![0x30, 0x00, 0x00]);
        let state = fsm.on_received(&[0x21, 7, 8, 9, 10]).unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(fsm.received(), &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn receive_wrong_cf_sequence() {
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 8, 0, 0);
        let _ = fsm.on_received(&[0x10, 0x0A, 1, 2, 3, 4, 5, 6]);
        let state = fsm.on_received(&[0x23, 7, 8, 9, 10]).unwrap();
        assert_eq!(state, MsgState::ErrUnexpectedSequenceNo);
    }

    #[test]
    fn receive_unexpected_fc() {
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 8, 0, 0);
        let state = fsm.on_received(&[0x30, 0x00, 0x00]).unwrap();
        assert_eq!(state, MsgState::ErrReceivedUnexpectedFCFrame);
    }

    #[test]
    fn receive_fc_overflow_and_wait() {
        let data: Vec<u8> = (1..=20).collect();
        // Overflow
        let mut fsm = IsoTpFsm::new(data.clone(), true, 8, 0, 0);
        let _ = fsm.next_frame();
        let state = fsm.on_received(&[0x32, 0x00, 0x00]).unwrap();
        assert_eq!(state, MsgState::ErrFlowControlOverflow);
        let mut fsm = IsoTpFsm::new(data, true, 8, 0, 0);
        let _ = fsm.next_frame();
        let state = fsm.on_received(&[0x31, 0x00, 0x00]).unwrap();
        assert_eq!(state, MsgState::PendingRcvFC);
    }

    #[test]
    fn canfd_long_sf_send() {
        let data: Vec<u8> = (1..=10).collect();
        let mut fsm = IsoTpFsm::new(data, true, 64, 0, 0);
        assert_eq!(fsm.frame_type(), FrameType::SF);
        let (state, frame) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingRcv);
        assert_eq!(
            frame.unwrap(),
            vec![0x00, 0x0A, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        );
    }

    #[test]
    fn canfd_long_ff_send() {
        let data: Vec<u8> = (1..=5000u32).map(|i| (i & 0xFF) as u8).collect();
        let mut fsm = IsoTpFsm::new(data, true, 64, 0, 0);
        let (state, frame) = fsm.next_frame();
        assert_eq!(state, MsgState::PendingRcvFC);
        let frame = frame.unwrap();
        assert_eq!(frame.len(), 64);
        assert_eq!(&frame[..6], &[0x10, 0x00, 0x00, 0x00, 0x13, 0x88]);
        assert_eq!(frame[6], 0x01);
    }

    #[test]
    fn canfd_long_frames_receive() {
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 64, 0, 0);
        let state = fsm
            .on_received(&[0x00, 0x0A, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(fsm.received().len(), 10);
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 64, 0, 0);
        let state = fsm
            .on_received(&[0x10, 0x00, 0x00, 0x00, 0x13, 0x88, 1, 2])
            .unwrap();
        assert_eq!(state, MsgState::PendingSndFC);
    }

    #[test]
    fn st_min_conversion() {
        assert_eq!(IsoTpFsm::st_min_to_micro_secs(0x00), 0);
        assert_eq!(IsoTpFsm::st_min_to_micro_secs(0x05), 5000);
        assert_eq!(IsoTpFsm::st_min_to_micro_secs(0x7F), 127_000);
        assert_eq!(IsoTpFsm::st_min_to_micro_secs(0xF1), 100);
        assert_eq!(IsoTpFsm::st_min_to_micro_secs(0xF3), 300);
        assert_eq!(IsoTpFsm::st_min_to_micro_secs(0xF9), 900);
        assert_eq!(IsoTpFsm::st_min_to_micro_secs(0x80), 0);
    }

    #[test]
    fn receive_too_short_errors() {
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 8, 0, 0);
        assert!(fsm.on_received(&[]).is_err());
        assert!(fsm.on_received(&[0x10]).is_err());
        assert!(fsm.on_received(&[0x00]).is_err());
        let data: Vec<u8> = (1..=20).collect();
        let mut fsm = IsoTpFsm::new(data, true, 8, 0, 0);
        let _ = fsm.next_frame();
        assert!(fsm.on_received(&[0x30, 0x00]).is_err());
    }

    #[test]
    fn receive_flexray_strips_header() {
        let mut fsm = IsoTpFsm::new(vec![0x22], true, 8, 0, 0);
        let state = fsm
            .on_received_fr(&[0x00, 0x01, 0x00, 0x02, 0x03, 0x22, 0xF1, 0x90])
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(fsm.received(), &[0x22, 0xF1, 0x90]);
        assert!(fsm.on_received_fr(&[1, 2, 3]).is_err());
    }
}
