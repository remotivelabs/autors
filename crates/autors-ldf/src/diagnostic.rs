//! Standard LIN node-configuration diagnostic payloads.

use crate::error::{Error, Result};

/// Master request frame identifier.
pub const MASTER_REQUEST_FRAME_ID: u8 = 0x3c;
/// Slave response frame identifier.
pub const SLAVE_RESPONSE_FRAME_ID: u8 = 0x3d;
/// Reserved node address.
pub const NAD_RESERVED: u8 = 0x00;
/// Functional node address.
pub const NAD_FUNCTIONAL: u8 = 0x7e;
/// Broadcast node address.
pub const NAD_BROADCAST: u8 = 0x7f;

/// Assign NAD service identifier.
pub const SID_ASSIGN_NAD: u8 = 0xb0;
/// Assign frame ID service identifier.
pub const SID_ASSIGN_FRAME_ID: u8 = 0xb1;
/// Read by identifier service identifier.
pub const SID_READ_BY_ID: u8 = 0xb2;
/// Conditional change NAD service identifier.
pub const SID_CONDITIONAL_CHANGE_NAD: u8 = 0xb3;
/// Data dump service identifier.
pub const SID_DATA_DUMP: u8 = 0xb4;
/// Save configuration service identifier.
pub const SID_SAVE_CONFIGURATION: u8 = 0xb6;
/// Assign frame ID range service identifier.
pub const SID_ASSIGN_FRAME_ID_RANGE: u8 = 0xb7;
/// Reserved service identifier.
pub const SID_RESERVED: u8 = 0xb5;
/// Read-by-identifier selector for the product ID.
pub const READ_BY_ID_PRODUCT_ID: u8 = 0;
/// Read-by-identifier selector for the serial number.
pub const READ_BY_ID_SERIAL_NUMBER: u8 = 1;
/// PCI type nibble for a single frame.
pub const PCI_SINGLE_FRAME: u8 = 0;
/// PCI type nibble for the first frame of a multi-frame transfer.
pub const PCI_FIRST_FRAME: u8 = 1;
/// PCI type nibble for a consecutive frame.
pub const PCI_CONSECUTIVE_FRAME: u8 = 2;

/// Positive-response service identifier for `sid`.
pub const fn response_sid(sid: u8) -> u8 {
    sid.wrapping_add(0x40)
}

/// Constructs the protocol-control-information byte.
pub const fn pci_byte(frame_type: u8, length: u8) -> u8 {
    (length & 0x0f) | (frame_type << 4)
}

/// Builders for single-frame LIN node-configuration requests.
pub struct DiagnosticRequest;

impl DiagnosticRequest {
    /// Builds an arbitrary eight-byte request payload.
    pub const fn raw(nad: u8, pci: u8, sid: u8, data: [u8; 5]) -> [u8; 8] {
        [nad, pci, sid, data[0], data[1], data[2], data[3], data[4]]
    }

    /// Assigns `new_nad` to a node selected by product ID.
    pub const fn assign_nad(
        initial_nad: u8,
        supplier_id: u16,
        function_id: u16,
        new_nad: u8,
    ) -> [u8; 8] {
        Self::raw(
            initial_nad,
            pci_byte(0, 6),
            SID_ASSIGN_NAD,
            [
                supplier_id as u8,
                (supplier_id >> 8) as u8,
                function_id as u8,
                (function_id >> 8) as u8,
                new_nad,
            ],
        )
    }

    /// Builds a conditional-change-NAD request.
    pub const fn conditional_change_nad(
        nad: u8,
        identifier: u8,
        byte: u8,
        mask: u8,
        invert: u8,
        new_nad: u8,
    ) -> [u8; 8] {
        Self::raw(
            nad,
            pci_byte(0, 6),
            SID_CONDITIONAL_CHANGE_NAD,
            [identifier, byte, mask, invert, new_nad],
        )
    }

    /// Builds a data-dump request.
    pub const fn data_dump(nad: u8, data: [u8; 5]) -> [u8; 8] {
        Self::raw(nad, pci_byte(0, 6), SID_DATA_DUMP, data)
    }

    /// Builds a save-configuration request.
    pub const fn save_configuration(nad: u8) -> [u8; 8] {
        Self::raw(nad, pci_byte(0, 1), SID_SAVE_CONFIGURATION, [0xff; 5])
    }

    /// Builds an assign-frame-ID-range request.
    pub const fn assign_frame_id_range(
        nad: u8,
        start_index: u8,
        protected_ids: [u8; 4],
    ) -> [u8; 8] {
        Self::raw(
            nad,
            pci_byte(0, 6),
            SID_ASSIGN_FRAME_ID_RANGE,
            [
                start_index,
                protected_ids[0],
                protected_ids[1],
                protected_ids[2],
                protected_ids[3],
            ],
        )
    }

    /// Builds a read-by-identifier request.
    pub const fn read_by_id(
        nad: u8,
        identifier: u8,
        supplier_id: u16,
        function_id: u16,
    ) -> [u8; 8] {
        Self::raw(
            nad,
            pci_byte(0, 6),
            SID_READ_BY_ID,
            [
                identifier,
                supplier_id as u8,
                (supplier_id >> 8) as u8,
                function_id as u8,
                (function_id >> 8) as u8,
            ],
        )
    }
}

/// Decoded single-frame diagnostic response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticResponse {
    /// Node address.
    pub nad: u8,
    /// Protocol-control-information byte.
    pub pci: u8,
    /// Response service identifier.
    pub response_sid: u8,
    /// Service-specific data bytes.
    pub data: [u8; 5],
}

impl DiagnosticResponse {
    /// Decodes an eight-byte diagnostic response payload.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let bytes: &[u8; 8] = payload.try_into().map_err(|_| {
            Error::Codec(format!(
                "diagnostic response must contain 8 bytes, got {}",
                payload.len()
            ))
        })?;
        Ok(Self {
            nad: bytes[0],
            pci: bytes[1],
            response_sid: bytes[2],
            data: [bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_request_vectors() {
        assert_eq!(
            DiagnosticRequest::assign_nad(0, 0x7fff, 0xffff, 1),
            [0x00, 0x06, 0xb0, 0xff, 0x7f, 0xff, 0xff, 0x01]
        );
        assert_eq!(
            DiagnosticRequest::save_configuration(1),
            [0x01, 0x01, 0xb6, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
    }

    #[test]
    fn decodes_response() {
        let response = DiagnosticResponse::decode(&[0, 1, 0xf0, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(response.nad, 0);
        assert_eq!(response.response_sid, 0xf0);
        assert_eq!(response.data, [2, 3, 4, 5, 6]);
        assert!(DiagnosticResponse::decode(&[0; 7]).is_err());
    }
}
