//! ISO 17987-2 diagnostic transport frame codec and state machine.
//!
//! LIN transport uses the same SF/FF/CF segmentation idea as ISO-TP, but each
//! eight-byte diagnostic frame starts with a node address (NAD), carries no
//! flow-control PDU, and is scheduled by the LIN commander.

use std::collections::VecDeque;

use autors_lin::device::LinFrame;

use crate::error::{Error, Result};
use crate::isotp::{MsgState, MAX_MSG_LEN_LIN};

/// LIN commander request frame identifier (MRF).
pub const MASTER_REQUEST_FRAME_ID: u8 = 0x3c;
/// LIN responder response frame identifier (SRF).
pub const SLAVE_RESPONSE_FRAME_ID: u8 = 0x3d;
/// Functional node address. Functional requests must fit in one frame.
pub const NAD_FUNCTIONAL: u8 = 0x7e;
/// Broadcast node address.
pub const NAD_BROADCAST: u8 = 0x7f;
/// Fixed diagnostic-frame data length.
pub const LIN_TP_FRAME_LEN: usize = 8;
/// Payload capacity of a single or consecutive frame.
pub const LIN_TP_CF_DATA_LEN: usize = 6;
/// Payload capacity of a first frame.
pub const LIN_TP_FF_DATA_LEN: usize = 5;

/// LIN transport PDU type encoded in the high nibble of the PCI byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinTpFrameType {
    /// A complete message of one to six bytes.
    Single,
    /// The first five bytes of a message of seven to 4095 bytes.
    First,
    /// A subsequent six-byte segment, with a modulo-16 sequence number.
    Consecutive,
}

/// Decoded eight-byte LIN transport PDU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinTpPdu {
    /// A complete, unsegmented message.
    Single { nad: u8, data: Vec<u8> },
    /// The start of a segmented message.
    First {
        nad: u8,
        total_len: u16,
        data: Vec<u8>,
    },
    /// A segment following a first frame.
    Consecutive {
        nad: u8,
        sequence: u8,
        data: Vec<u8>,
    },
}

impl LinTpPdu {
    /// Returns the PDU's node address.
    pub fn nad(&self) -> u8 {
        match self {
            Self::Single { nad, .. } | Self::First { nad, .. } | Self::Consecutive { nad, .. } => {
                *nad
            }
        }
    }

    /// Returns the decoded frame type.
    pub fn frame_type(&self) -> LinTpFrameType {
        match self {
            Self::Single { .. } => LinTpFrameType::Single,
            Self::First { .. } => LinTpFrameType::First,
            Self::Consecutive { .. } => LinTpFrameType::Consecutive,
        }
    }

    /// Decodes one complete eight-byte diagnostic frame data field.
    pub fn decode(frame: &[u8]) -> Result<Self> {
        if frame.len() != LIN_TP_FRAME_LEN {
            return Err(Error::Protocol(format!(
                "LIN TP frame must contain {LIN_TP_FRAME_LEN} bytes, got {}",
                frame.len()
            )));
        }
        let nad = frame[0];
        let pci = frame[1];
        match pci >> 4 {
            0 => {
                let len = usize::from(pci & 0x0f);
                if !(1..=LIN_TP_CF_DATA_LEN).contains(&len) {
                    return Err(Error::Protocol(format!(
                        "LIN TP single-frame length must be 1..=6, got {len}"
                    )));
                }
                Ok(Self::Single {
                    nad,
                    data: frame[2..2 + len].to_vec(),
                })
            }
            1 => {
                let total_len = (u16::from(pci & 0x0f) << 8) | u16::from(frame[2]);
                if !(7..=MAX_MSG_LEN_LIN as u16).contains(&total_len) {
                    return Err(Error::Protocol(format!(
                        "LIN TP first-frame length must be 7..={MAX_MSG_LEN_LIN}, got {total_len}"
                    )));
                }
                Ok(Self::First {
                    nad,
                    total_len,
                    data: frame[3..].to_vec(),
                })
            }
            2 => Ok(Self::Consecutive {
                nad,
                sequence: pci & 0x0f,
                data: frame[2..].to_vec(),
            }),
            kind => Err(Error::Protocol(format!(
                "unsupported LIN TP PCI type 0x{kind:X}"
            ))),
        }
    }

    /// Encodes one PDU, padding unused data bytes with `fill_byte`.
    pub fn encode(&self, fill_byte: u8) -> Result<[u8; LIN_TP_FRAME_LEN]> {
        let mut frame = [fill_byte; LIN_TP_FRAME_LEN];
        frame[0] = self.nad();
        match self {
            Self::Single { data, .. } => {
                if !(1..=LIN_TP_CF_DATA_LEN).contains(&data.len()) {
                    return Err(Error::Protocol(format!(
                        "LIN TP single-frame payload must be 1..=6 bytes, got {}",
                        data.len()
                    )));
                }
                frame[1] = data.len() as u8;
                frame[2..2 + data.len()].copy_from_slice(data);
            }
            Self::First {
                total_len, data, ..
            } => {
                if !(7..=MAX_MSG_LEN_LIN as u16).contains(total_len) {
                    return Err(Error::Protocol(format!(
                        "LIN TP first-frame length must be 7..={MAX_MSG_LEN_LIN}, got {total_len}"
                    )));
                }
                if data.len() != LIN_TP_FF_DATA_LEN {
                    return Err(Error::Protocol(format!(
                        "LIN TP first frame must carry {LIN_TP_FF_DATA_LEN} bytes, got {}",
                        data.len()
                    )));
                }
                frame[1] = 0x10 | ((*total_len >> 8) as u8 & 0x0f);
                frame[2] = *total_len as u8;
                frame[3..].copy_from_slice(data);
            }
            Self::Consecutive { sequence, data, .. } => {
                if data.is_empty() || data.len() > LIN_TP_CF_DATA_LEN {
                    return Err(Error::Protocol(format!(
                        "LIN TP consecutive-frame payload must be 1..=6 bytes, got {}",
                        data.len()
                    )));
                }
                frame[1] = 0x20 | (sequence & 0x0f);
                frame[2..2 + data.len()].copy_from_slice(data);
            }
        }
        Ok(frame)
    }
}

#[derive(Debug, Clone)]
enum LinTpMode {
    Sender {
        frames: VecDeque<[u8; LIN_TP_FRAME_LEN]>,
    },
    Receiver {
        expected_nad: Option<u8>,
        total_len: Option<usize>,
        next_sequence: u8,
    },
}

/// Pure segmentation/reassembly state machine for LIN transport messages.
#[derive(Debug, Clone)]
pub struct LinTpFsm {
    mode: LinTpMode,
    received: Vec<u8>,
    state: MsgState,
}

impl LinTpFsm {
    /// Creates a sender and pre-segments `data` into fixed eight-byte PDUs.
    pub fn new_sender(nad: u8, data: Vec<u8>, fill_byte: u8) -> Result<Self> {
        let frames = segment_message(nad, &data, fill_byte)?.into();
        Ok(Self {
            mode: LinTpMode::Sender { frames },
            received: Vec::new(),
            state: MsgState::PendingSnd,
        })
    }

    /// Creates a receiver. `None` accepts any response NAD.
    pub fn new_receiver(expected_nad: Option<u8>) -> Self {
        Self {
            mode: LinTpMode::Receiver {
                expected_nad,
                total_len: None,
                next_sequence: 1,
            },
            received: Vec::new(),
            state: MsgState::PendingRcv,
        }
    }

    /// Returns the current transfer state.
    pub fn state(&self) -> MsgState {
        self.state
    }

    /// Returns the reassembled message bytes.
    pub fn received(&self) -> &[u8] {
        &self.received
    }

    /// Takes the reassembled message bytes.
    pub fn take_received(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.received)
    }

    /// Returns the next sender PDU. The state becomes `Success` with the last PDU.
    pub fn next_frame(&mut self) -> (MsgState, Option<[u8; LIN_TP_FRAME_LEN]>) {
        let LinTpMode::Sender { frames } = &mut self.mode else {
            return (self.state, None);
        };
        let frame = frames.pop_front();
        if frame.is_some() && frames.is_empty() {
            self.state = MsgState::Success;
        }
        (self.state, frame)
    }

    /// Feeds one eight-byte PDU into a receiver.
    ///
    /// Frames addressed to another NAD and unexpected CF PDUs without an
    /// active segmented transfer are ignored.
    pub fn on_received(&mut self, frame: &[u8]) -> Result<MsgState> {
        let pdu = LinTpPdu::decode(frame)?;
        let LinTpMode::Receiver {
            expected_nad,
            total_len,
            next_sequence,
        } = &mut self.mode
        else {
            return Err(Error::Protocol(
                "cannot receive a LIN TP PDU with a sender state machine".to_string(),
            ));
        };
        if expected_nad.is_some_and(|nad| nad != pdu.nad()) {
            return Ok(self.state);
        }
        if total_len.is_some() && pdu.nad() == NAD_FUNCTIONAL {
            return Ok(self.state);
        }
        match pdu {
            LinTpPdu::Single { data, .. } => {
                self.received = data;
                *total_len = None;
                *next_sequence = 1;
                self.state = MsgState::Success;
            }
            LinTpPdu::First {
                total_len: len,
                data,
                ..
            } => {
                self.received = data;
                *total_len = Some(usize::from(len));
                *next_sequence = 1;
                self.state = MsgState::PendingRcv;
            }
            LinTpPdu::Consecutive { sequence, data, .. } => {
                let Some(message_len) = *total_len else {
                    return Ok(self.state);
                };
                if sequence != *next_sequence {
                    self.state = MsgState::ErrUnexpectedSequenceNo;
                    return Ok(self.state);
                }
                *next_sequence = next_sequence.wrapping_add(1) & 0x0f;
                let remaining = message_len.saturating_sub(self.received.len());
                let count = remaining.min(data.len());
                self.received.extend_from_slice(&data[..count]);
                if self.received.len() == message_len {
                    *total_len = None;
                    self.state = MsgState::Success;
                }
            }
        }
        Ok(self.state)
    }

    /// Feeds the data field of a complete LIN frame into the receiver.
    pub fn on_received_lin(&mut self, frame: &LinFrame) -> Result<MsgState> {
        self.on_received(&frame.data)
    }
}

/// Segments a complete LIN transport message into eight-byte PDUs.
pub fn segment_message(nad: u8, data: &[u8], fill_byte: u8) -> Result<Vec<[u8; LIN_TP_FRAME_LEN]>> {
    if data.is_empty() {
        return Err(Error::Protocol(
            "LIN TP message must contain at least one byte".to_string(),
        ));
    }
    if data.len() > MAX_MSG_LEN_LIN as usize {
        return Err(Error::Protocol(format!(
            "LIN TP message length {} exceeds {MAX_MSG_LEN_LIN}",
            data.len()
        )));
    }
    if nad == NAD_FUNCTIONAL && data.len() > LIN_TP_CF_DATA_LEN {
        return Err(Error::Protocol(
            "functional LIN TP requests must use a single frame".to_string(),
        ));
    }
    if data.len() <= LIN_TP_CF_DATA_LEN {
        return Ok(vec![LinTpPdu::Single {
            nad,
            data: data.to_vec(),
        }
        .encode(fill_byte)?]);
    }

    let mut frames = Vec::with_capacity(1 + data.len().div_ceil(LIN_TP_CF_DATA_LEN));
    frames.push(
        LinTpPdu::First {
            nad,
            total_len: data.len() as u16,
            data: data[..LIN_TP_FF_DATA_LEN].to_vec(),
        }
        .encode(fill_byte)?,
    );
    let mut sequence = 1u8;
    for chunk in data[LIN_TP_FF_DATA_LEN..].chunks(LIN_TP_CF_DATA_LEN) {
        frames.push(
            LinTpPdu::Consecutive {
                nad,
                sequence,
                data: chunk.to_vec(),
            }
            .encode(fill_byte)?,
        );
        sequence = sequence.wrapping_add(1) & 0x0f;
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_frame_vector_and_padding() {
        let frames = segment_message(0x12, &[0x22, 0xf1, 0x90], 0xff).unwrap();
        assert_eq!(
            frames,
            vec![[0x12, 0x03, 0x22, 0xf1, 0x90, 0xff, 0xff, 0xff]]
        );
        assert_eq!(
            LinTpPdu::decode(&frames[0]).unwrap(),
            LinTpPdu::Single {
                nad: 0x12,
                data: vec![0x22, 0xf1, 0x90]
            }
        );
    }

    #[test]
    fn segmented_message_roundtrip() {
        let message: Vec<u8> = (0..=31).collect();
        let frames = segment_message(0x22, &message, 0xaa).unwrap();
        assert_eq!(&frames[0][..3], &[0x22, 0x10, 0x20]);
        assert_eq!(frames[1][1], 0x21);
        assert_eq!(frames.last().unwrap()[1], 0x25);

        let mut receiver = LinTpFsm::new_receiver(Some(0x22));
        for frame in frames {
            receiver.on_received(&frame).unwrap();
        }
        assert_eq!(receiver.state(), MsgState::Success);
        assert_eq!(receiver.received(), message);
    }

    #[test]
    fn sender_state_finishes_with_last_frame() {
        let mut sender = LinTpFsm::new_sender(1, vec![1; 7], 0xff).unwrap();
        let (state, first) = sender.next_frame();
        assert_eq!(state, MsgState::PendingSnd);
        assert!(first.is_some());
        let (state, second) = sender.next_frame();
        assert_eq!(state, MsgState::Success);
        assert!(second.is_some());
        assert!(sender.next_frame().1.is_none());
    }

    #[test]
    fn rejects_bad_lengths_and_functional_segmentation() {
        assert!(segment_message(1, &[], 0xff).is_err());
        assert!(segment_message(NAD_FUNCTIONAL, &[0; 7], 0xff).is_err());
        assert!(LinTpPdu::decode(&[0; 7]).is_err());
        assert!(LinTpPdu::decode(&[1, 7, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(LinTpPdu::decode(&[1, 0x10, 6, 0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn unexpected_sequence_is_reported() {
        let mut receiver = LinTpFsm::new_receiver(Some(1));
        receiver.on_received(&[1, 0x10, 7, 1, 2, 3, 4, 5]).unwrap();
        let state = receiver
            .on_received(&[1, 0x22, 6, 7, 0xff, 0xff, 0xff, 0xff])
            .unwrap();
        assert_eq!(state, MsgState::ErrUnexpectedSequenceNo);
    }

    #[test]
    fn maximum_message_wraps_sequence_and_roundtrips() {
        let message: Vec<u8> = (0..MAX_MSG_LEN_LIN).map(|value| value as u8).collect();
        let frames = segment_message(1, &message, 0xff).unwrap();
        assert_eq!(frames.len(), 683);
        assert_eq!(frames[15][1], 0x2f);
        assert_eq!(frames[16][1], 0x20);

        let mut receiver = LinTpFsm::new_receiver(Some(1));
        for frame in frames {
            receiver.on_received(&frame).unwrap();
        }
        assert_eq!(receiver.state(), MsgState::Success);
        assert_eq!(receiver.received(), message);
        assert!(segment_message(1, &vec![0; MAX_MSG_LEN_LIN as usize + 1], 0xff).is_err());
    }
}
