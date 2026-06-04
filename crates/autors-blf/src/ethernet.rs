//! Ethernet BLF event objects.

use crate::error::Result;
use crate::objects::{
    parse_err, put_i16, put_u16, put_u32, put_u64, put_u8, LObj, Reader, LOBJ_SIZE,
};

fn object(bytes: &[u8], base: u64, minimum: usize) -> Result<(LObj, &[u8])> {
    let header = LObj::parse(bytes, base)?;
    let size = header.base.object_size as usize;
    if size < minimum || size > bytes.len() {
        return parse_err(
            base,
            format!("Ethernet object size {size} is out of bounds"),
        );
    }
    Ok((header, &bytes[..size]))
}

fn payload_and_trailing(
    bytes: &[u8],
    fixed: usize,
    length: usize,
    base: u64,
) -> Result<(&[u8], &[u8])> {
    let end = fixed
        .checked_add(length)
        .ok_or_else(|| crate::error::Error::Parse {
            offset: base,
            message: "Ethernet payload length overflow".into(),
        })?;
    if end > bytes.len() {
        return parse_err(base, "Ethernet payload exceeds its object size");
    }
    Ok((&bytes[fixed..end], &bytes[end..]))
}

/// Legacy Ethernet frame with source/destination MAC addresses and VLAN fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthernetFrame {
    pub header: LObj,
    pub source_address: [u8; 6],
    pub channel: u16,
    pub destination_address: [u8; 6],
    pub dir: u16,
    pub ether_type: u16,
    pub tpid: u16,
    pub tci: u16,
    pub reserved: u64,
    pub payload: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl EthernetFrame {
    pub const FIXED_SIZE: usize = 64;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, bytes) = object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let mut source_address = [0; 6];
        source_address.copy_from_slice(r.take(6)?);
        let channel = r.u16()?;
        let mut destination_address = [0; 6];
        destination_address.copy_from_slice(r.take(6)?);
        let dir = r.u16()?;
        let ether_type = r.u16()?;
        let tpid = r.u16()?;
        let tci = r.u16()?;
        let payload_length = r.u16()? as usize;
        let reserved = r.u64()?;
        let (payload, trailing) =
            payload_and_trailing(bytes, Self::FIXED_SIZE, payload_length, base)?;
        Ok(Self {
            header,
            source_address,
            channel,
            destination_address,
            dir,
            ether_type,
            tpid,
            tci,
            reserved,
            payload: payload.to_vec(),
            trailing: trailing.to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size =
            (Self::FIXED_SIZE + self.payload.len() + self.trailing.len()) as u32;
        header.write_to(out);
        out.extend_from_slice(&self.source_address);
        put_u16(out, self.channel);
        out.extend_from_slice(&self.destination_address);
        put_u16(out, self.dir);
        put_u16(out, self.ether_type);
        put_u16(out, self.tpid);
        put_u16(out, self.tci);
        put_u16(out, self.payload.len() as u16);
        put_u64(out, self.reserved);
        out.extend_from_slice(&self.payload);
        out.extend_from_slice(&self.trailing);
    }
}

/// Ethernet receive error and the captured frame bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthernetRxError {
    pub header: LObj,
    pub struct_length: u16,
    pub channel: u16,
    pub dir: u16,
    pub hardware_channel: u16,
    pub fcs: u32,
    pub reserved: u16,
    pub error: u32,
    pub frame_data: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl EthernetRxError {
    pub const FIXED_SIZE: usize = 52;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, bytes) = object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let struct_length = r.u16()?;
        let channel = r.u16()?;
        let dir = r.u16()?;
        let hardware_channel = r.u16()?;
        let fcs = r.u32()?;
        let frame_data_length = r.u16()? as usize;
        let reserved = r.u16()?;
        let error = r.u32()?;
        let (frame_data, trailing) =
            payload_and_trailing(bytes, Self::FIXED_SIZE, frame_data_length, base)?;
        Ok(Self {
            header,
            struct_length,
            channel,
            dir,
            hardware_channel,
            fcs,
            reserved,
            error,
            frame_data: frame_data.to_vec(),
            trailing: trailing.to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size =
            (Self::FIXED_SIZE + self.frame_data.len() + self.trailing.len()) as u32;
        header.write_to(out);
        put_u16(out, self.struct_length);
        put_u16(out, self.channel);
        put_u16(out, self.dir);
        put_u16(out, self.hardware_channel);
        put_u32(out, self.fcs);
        put_u16(out, self.frame_data.len() as u16);
        put_u16(out, self.reserved);
        put_u32(out, self.error);
        out.extend_from_slice(&self.frame_data);
        out.extend_from_slice(&self.trailing);
    }
}

/// Ethernet link and PHY status. Version 2 adds the two reserved words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthernetStatus {
    pub header: LObj,
    pub channel: u16,
    pub flags: u16,
    pub link_status: u8,
    pub ethernet_phy: u8,
    pub duplex: u8,
    pub mdi: u8,
    pub connector: u8,
    pub clock_mode: u8,
    pub pairs: u8,
    pub hardware_channel: u8,
    pub bitrate: u32,
    pub reserved1: Option<u32>,
    pub reserved2: Option<u32>,
    pub trailing: Vec<u8>,
}

impl EthernetStatus {
    pub const V1_SIZE: usize = 48;
    pub const V2_SIZE: usize = 56;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, bytes) = object(bytes, base, Self::V1_SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let channel = r.u16()?;
        let flags = r.u16()?;
        let link_status = r.u8()?;
        let ethernet_phy = r.u8()?;
        let duplex = r.u8()?;
        let mdi = r.u8()?;
        let connector = r.u8()?;
        let clock_mode = r.u8()?;
        let pairs = r.u8()?;
        let hardware_channel = r.u8()?;
        let bitrate = r.u32()?;
        let (reserved1, reserved2, trailing_start) = if bytes.len() >= Self::V2_SIZE {
            (Some(r.u32()?), Some(r.u32()?), Self::V2_SIZE)
        } else {
            (None, None, Self::V1_SIZE)
        };
        Ok(Self {
            header,
            channel,
            flags,
            link_status,
            ethernet_phy,
            duplex,
            mdi,
            connector,
            clock_mode,
            pairs,
            hardware_channel,
            bitrate,
            reserved1,
            reserved2,
            trailing: bytes[trailing_start..].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let extended = self.reserved1.is_some() || self.reserved2.is_some();
        let fixed = if extended {
            Self::V2_SIZE
        } else {
            Self::V1_SIZE
        };
        let mut header = self.header.clone();
        header.base.object_size = (fixed + self.trailing.len()) as u32;
        header.write_to(out);
        put_u16(out, self.channel);
        put_u16(out, self.flags);
        put_u8(out, self.link_status);
        put_u8(out, self.ethernet_phy);
        put_u8(out, self.duplex);
        put_u8(out, self.mdi);
        put_u8(out, self.connector);
        put_u8(out, self.clock_mode);
        put_u8(out, self.pairs);
        put_u8(out, self.hardware_channel);
        put_u32(out, self.bitrate);
        if extended {
            put_u32(out, self.reserved1.unwrap_or_default());
            put_u32(out, self.reserved2.unwrap_or_default());
        }
        out.extend_from_slice(&self.trailing);
    }
}

/// Ethernet hardware counters and signal-quality index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthernetStatistic {
    pub header: LObj,
    pub channel: u16,
    pub reserved1: u16,
    pub reserved2: u32,
    pub receive_ok: u64,
    pub transmit_ok: u64,
    pub receive_error: u64,
    pub transmit_error: u64,
    pub receive_bytes: u64,
    pub transmit_bytes: u64,
    pub receive_no_buffer: u64,
    pub sqi: i16,
    pub hardware_channel: u16,
    pub reserved3: u32,
}

impl EthernetStatistic {
    pub const SIZE: usize = 104;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, _) = object(bytes, base, Self::SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            channel: r.u16()?,
            reserved1: r.u16()?,
            reserved2: r.u32()?,
            receive_ok: r.u64()?,
            transmit_ok: r.u64()?,
            receive_error: r.u64()?,
            transmit_error: r.u64()?,
            receive_bytes: r.u64()?,
            transmit_bytes: r.u64()?,
            receive_no_buffer: r.u64()?,
            sqi: r.i16()?,
            hardware_channel: r.u16()?,
            reserved3: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u16(out, self.reserved1);
        put_u32(out, self.reserved2);
        put_u64(out, self.receive_ok);
        put_u64(out, self.transmit_ok);
        put_u64(out, self.receive_error);
        put_u64(out, self.transmit_error);
        put_u64(out, self.receive_bytes);
        put_u64(out, self.transmit_bytes);
        put_u64(out, self.receive_no_buffer);
        put_i16(out, self.sqi);
        put_u16(out, self.hardware_channel);
        put_u32(out, self.reserved3);
    }
}

/// Extended or forwarded Ethernet frame/error; all four IDs share this layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthernetFrameEx {
    pub header: LObj,
    pub struct_length: u16,
    pub flags: u16,
    pub channel: u16,
    pub hardware_channel: u16,
    pub frame_duration: u64,
    pub frame_checksum: u32,
    pub dir: u16,
    pub frame_handle: u32,
    /// Reserved for frame IDs and an error code for error IDs.
    pub status: u32,
    pub frame_data: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl EthernetFrameEx {
    pub const FIXED_SIZE: usize = 64;

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let (header, bytes) = object(bytes, base, Self::FIXED_SIZE)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let struct_length = r.u16()?;
        let flags = r.u16()?;
        let channel = r.u16()?;
        let hardware_channel = r.u16()?;
        let frame_duration = r.u64()?;
        let frame_checksum = r.u32()?;
        let dir = r.u16()?;
        let frame_length = r.u16()? as usize;
        let frame_handle = r.u32()?;
        let status = r.u32()?;
        let (frame_data, trailing) =
            payload_and_trailing(bytes, Self::FIXED_SIZE, frame_length, base)?;
        Ok(Self {
            header,
            struct_length,
            flags,
            channel,
            hardware_channel,
            frame_duration,
            frame_checksum,
            dir,
            frame_handle,
            status,
            frame_data: frame_data.to_vec(),
            trailing: trailing.to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size =
            (Self::FIXED_SIZE + self.frame_data.len() + self.trailing.len()) as u32;
        header.write_to(out);
        put_u16(out, self.struct_length);
        put_u16(out, self.flags);
        put_u16(out, self.channel);
        put_u16(out, self.hardware_channel);
        put_u64(out, self.frame_duration);
        put_u32(out, self.frame_checksum);
        put_u16(out, self.dir);
        put_u16(out, self.frame_data.len() as u16);
        put_u32(out, self.frame_handle);
        put_u32(out, self.status);
        out.extend_from_slice(&self.frame_data);
        out.extend_from_slice(&self.trailing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::{ObjectFlags, ObjectType};

    #[test]
    fn legacy_frame_roundtrip() {
        let payload = vec![0x45, 0, 0, 20];
        let frame = EthernetFrame {
            header: LObj::new(
                ObjectType::EthernetFrame,
                ObjectFlags::TIME_ONE_NANS,
                42,
                (EthernetFrame::FIXED_SIZE + payload.len()) as u32,
            ),
            source_address: [0, 1, 2, 3, 4, 5],
            channel: 1,
            destination_address: [6, 7, 8, 9, 10, 11],
            dir: 1,
            ether_type: 0x0800,
            tpid: 0x8100,
            tci: 3,
            reserved: 0,
            payload,
            trailing: Vec::new(),
        };
        let mut bytes = Vec::new();
        frame.write_to(&mut bytes);
        assert_eq!(EthernetFrame::parse(&bytes, 0).unwrap(), frame);
    }

    #[test]
    fn extended_frame_roundtrip() {
        let data = vec![1, 2, 3];
        let frame = EthernetFrameEx {
            header: LObj::new(
                ObjectType::EthernetErrorForwarded,
                ObjectFlags::TIME_ONE_NANS,
                43,
                (EthernetFrameEx::FIXED_SIZE + data.len()) as u32,
            ),
            struct_length: 30,
            flags: 1,
            channel: 2,
            hardware_channel: 3,
            frame_duration: 100,
            frame_checksum: 0x1234,
            dir: 0,
            frame_handle: 9,
            status: 7,
            frame_data: data,
            trailing: Vec::new(),
        };
        let mut bytes = Vec::new();
        frame.write_to(&mut bytes);
        assert_eq!(EthernetFrameEx::parse(&bytes, 0).unwrap(), frame);
    }
}
