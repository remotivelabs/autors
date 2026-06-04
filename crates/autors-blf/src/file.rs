//! BLF file reading and writing: the top-level [`BlfFile`] type and the
//! container plumbing around it.
//! Reading: `parse(&[u8])` validates the 144-byte file header, scans the
//! top-level LOG_CONTAINER objects, decompresses and concatenates their
//! payloads into an uncompressed object stream, then parses the inner objects
//!   ([`crate::objects::Time::from_system_time`]).

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use autors_can::frame::{CanFrame, FrameType};
use autors_lin::device::{ChecksumType as LinChecksumType, LinFrame};

use crate::error::{Error, Result};
use crate::objects::{
    AppId, BlfObject, CanFdFlags, CanFdMessage, CanFlags, CanMessage, Header, LinMessage2,
    LogContainer, ObjectFlags, ObjectType, LOG_CONTAINER_SIZE,
};

const ZLIB_HEADER: [u8; 2] = [0x78, 0x01];

const MAX_CONTAINER_BUFFER: usize = 4 * 1024 * 1024;

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

pub(crate) fn zlib_deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data)
        .map_err(|e| Error::Compression(e.to_string()))?;
    let deflated = enc
        .finish()
        .map_err(|e| Error::Compression(e.to_string()))?;
    let mut out = Vec::with_capacity(ZLIB_HEADER.len() + deflated.len() + 4);
    out.extend_from_slice(&ZLIB_HEADER);
    out.extend_from_slice(&deflated);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    Ok(out)
}

pub(crate) fn zlib_inflate_limited(payload: &[u8], limit: usize) -> Result<Vec<u8>> {
    if payload.len() < 6 {
        return Err(Error::Compression(
            "zlib container payload is shorter than its header and checksum".into(),
        ));
    }
    let mut dec = flate2::read::ZlibDecoder::new(payload);
    let mut out = Vec::new();
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    dec.by_ref()
        .take(read_limit)
        .read_to_end(&mut out)
        .map_err(|e| Error::Compression(e.to_string()))?;
    if out.len() > limit {
        return Err(Error::Compression(format!(
            "zlib output exceeds configured limit of {limit} bytes"
        )));
    }
    Ok(out)
}

pub(crate) fn encode_container(buf: &[u8], compress: bool) -> Result<(Vec<u8>, u64)> {
    let (data, compression_method) = if compress {
        (zlib_deflate(buf)?, 2)
    } else {
        (buf.to_vec(), 0)
    };
    let uncompressed_size = u32::try_from(buf.len())
        .map_err(|_| Error::Write("log container exceeds the u32 size limit".into()))?;
    let lc = LogContainer::new_with_method(data.len(), compression_method, uncompressed_size);
    let mut out = Vec::with_capacity(LOG_CONTAINER_SIZE + data.len() + 3);
    lc.write_to(&mut out);
    out.extend_from_slice(&data);
    let pad = (lc.base.object_size & 3) as usize;
    out.resize(out.len() + pad, 0);
    Ok((out, LOG_CONTAINER_SIZE as u64 + buf.len() as u64))
}

fn flush_container(out: &mut Vec<u8>, buf: &[u8], compress: bool) -> Result<u64> {
    let (container, uncompressed_size) = encode_container(buf, compress)?;
    out.extend_from_slice(&container);
    Ok(uncompressed_size)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlfFile {
    pub header: Header,
    pub objects: Vec<BlfObject>,
    frame_time_base: Option<Duration>,
}

impl BlfFile {
    pub const CAN_DATA_FRAMES: [ObjectType; 4] = [
        ObjectType::CanMessage,
        ObjectType::CanMessage2,
        ObjectType::CanFdMessage,
        ObjectType::CanFdMessage64,
    ];

    pub const LIN_DATA_FRAMES: [ObjectType; 2] = [ObjectType::LinMessage, ObjectType::LinMessage2];

    pub fn new(
        compressed: bool,
        app_id: AppId,
        app_major: u8,
        app_minor: u8,
        app_build: u32,
    ) -> Self {
        BlfFile {
            header: Header::new(compressed, app_id, app_major, app_minor, app_build),
            objects: Vec::new(),
            frame_time_base: None,
        }
    }

    /// Appends any strongly typed or opaque BLF object.
    pub fn push_object(&mut self, object: impl Into<BlfObject>) {
        self.objects.push(object.into());
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        Self::parse_reader(std::io::Cursor::new(data))
    }

    /// Reads a BLF stream and collects its objects into memory.
    pub fn parse_reader(reader: impl Read) -> Result<Self> {
        let mut reader = crate::stream::BlfReader::new(reader)?;
        let header = reader.header().clone();
        let objects = reader.by_ref().collect::<Result<Vec<_>>>()?;
        Ok(Self {
            header,
            objects,
            frame_time_base: None,
        })
    }

    pub fn add_can_frame(
        &mut self,
        frame: &CanFrame,
        channel: u16,
        object_flags: ObjectFlags,
    ) -> Result<()> {
        let timestamp = self.frame_timestamp(frame.elapsed, object_flags)?;
        let object = if frame.frame_type.is_classic() {
            if frame.data.len() > 8 {
                return Err(Error::Write(format!(
                    "classic CAN frame data length {} exceeds 8",
                    frame.data.len()
                )));
            }
            BlfObject::CanMessage(CanMessage::new(
                object_flags,
                timestamp,
                channel,
                frame.id,
                &frame.data,
                frame.is_master_frame,
            ))
        } else {
            if frame.data.len() > 64 {
                return Err(Error::Write(format!(
                    "CAN FD frame data length {} exceeds 64",
                    frame.data.len()
                )));
            }
            BlfObject::CanFdMessage(CanFdMessage::new(
                object_flags,
                timestamp,
                channel,
                frame.id,
                &frame.data,
                frame.is_master_frame,
                frame.frame_type.contains(FrameType::FD),
                frame.frame_type.contains(FrameType::BRS),
            ))
        };
        self.objects.push(object);
        Ok(())
    }

    fn frame_timestamp(&mut self, elapsed: Duration, object_flags: ObjectFlags) -> Result<u64> {
        let base = *self.frame_time_base.get_or_insert(elapsed);
        let Some(delta) = elapsed.checked_sub(base) else {
            return Err(Error::Write(
                "frame elapsed is earlier than the first frame's".into(),
            ));
        };
        let delta_ms = (delta.as_nanos() / 100) as f64 / 10_000.0;
        let scaled = if object_flags.contains(ObjectFlags::TIME_TEN_MICS) {
            delta_ms * 100.0
        } else {
            delta_ms * 1_000_000.0
        };
        let rounded = scaled.round_ties_even();
        if !(0.0..=u64::MAX as f64).contains(&rounded) {
            return Err(Error::Write(format!(
                "timestamp {scaled} is outside the u64 range"
            )));
        }
        Ok(rounded as u64)
    }

    pub fn enum_can_frames(&self, channel: i32, id: u32) -> Vec<CanFrame> {
        self.iter_objects(Some(&Self::CAN_DATA_FRAMES))
            .filter(|o| o.matches_can_filter(channel, id))
            .map(can_frame_from_object)
            .collect()
    }

    /// Adds a current-generation LIN message object to the file.
    pub fn add_lin_frame(
        &mut self,
        frame: &LinFrame,
        channel: u16,
        checksum_type: LinChecksumType,
        object_flags: ObjectFlags,
    ) -> Result<()> {
        if frame.id > 0x3f {
            return Err(Error::Write(format!(
                "LIN identifier 0x{:02X} exceeds 0x3F",
                frame.id
            )));
        }
        if frame.data.len() > 8 {
            return Err(Error::Write(format!(
                "LIN frame data length {} exceeds 8",
                frame.data.len()
            )));
        }
        let timestamp = self.frame_timestamp(frame.elapsed, object_flags)?;
        let checksum_model = match checksum_type {
            LinChecksumType::CalcChecksum => 0,
            LinChecksumType::CalcChecksumEnhanced => 1,
        };
        self.objects.push(BlfObject::LinMessage2(LinMessage2::new(
            object_flags,
            timestamp,
            channel,
            frame.id,
            &frame.data,
            frame.checksum(checksum_type),
            checksum_model,
            frame.is_master_frame,
        )));
        Ok(())
    }

    /// Enumerates decoded LIN data frames. A negative channel matches all
    /// channels; `u8::MAX` matches every LIN identifier.
    pub fn enum_lin_frames(&self, channel: i32, id: u8) -> Vec<LinFrame> {
        self.iter_objects(Some(&Self::LIN_DATA_FRAMES))
            .filter(|object| object.matches_lin_filter(channel, id))
            .filter_map(lin_frame_from_object)
            .collect()
    }
}

fn can_frame_from_object(obj: &BlfObject) -> CanFrame {
    match obj {
        BlfObject::CanMessage(m) => CanFrame::new(
            m.channel.to_string(),
            m.id,
            m.data[..(m.dlc as usize).min(8)].to_vec(),
            m.flags == CanFlags::TX,
            FrameType::CAN20B,
        ),
        BlfObject::CanMessage2(m) => CanFrame::new(
            m.channel.to_string(),
            m.id,
            m.data[..(m.dlc as usize).min(8)].to_vec(),
            m.flags == CanFlags::TX,
            FrameType::CAN20B,
        ),
        BlfObject::CanFdMessage(m) => {
            let mut frame_type = FrameType::CAN20B;
            if m.fd_flags.contains(CanFdFlags::EDL) {
                frame_type = frame_type | FrameType::FD;
            }
            if m.fd_flags.contains(CanFdFlags::BRS) {
                frame_type = frame_type | FrameType::BRS;
            }
            CanFrame::new(
                m.channel.to_string(),
                m.id,
                m.data[..(m.valid_data_bytes as usize).min(64)].to_vec(),
                m.flags == CanFlags::TX,
                frame_type,
            )
        }
        BlfObject::CanFdMessage64(m) => {
            let cs_flags = m.fd_flags_cs();
            let mut frame_type = FrameType::CAN20B;
            if cs_flags.contains(crate::objects::CanFd64Flags::EDL) {
                frame_type = frame_type | FrameType::FD;
            }
            if cs_flags.contains(crate::objects::CanFd64Flags::BRS) {
                frame_type = frame_type | FrameType::BRS;
            }
            CanFrame::new(
                m.channel.to_string(),
                m.id,
                m.data.clone(),
                m.dir > 0,
                frame_type,
            )
        }
        _ => CanFrame::new("0", 0, Vec::new(), false, FrameType::CAN20B),
    }
}

fn lin_frame_from_object(object: &BlfObject) -> Option<LinFrame> {
    let (header, channel, id, dlc, data, dir) = match object {
        BlfObject::LinMessage(message) => (
            &message.header,
            message.channel,
            message.id,
            message.dlc,
            &message.data,
            message.dir,
        ),
        BlfObject::LinMessage2(message) => (
            &message.header,
            message.channel,
            message.id,
            message.dlc,
            &message.data,
            message.dir,
        ),
        _ => return None,
    };
    let mut frame = LinFrame::new(
        &channel.to_string(),
        id,
        data[..usize::from(dlc).min(8)].to_vec(),
        dir != 0,
    );
    frame.elapsed = Duration::from_secs_f64(header.timestamp_seconds());
    Some(frame)
}

impl BlfFile {
    pub fn write(&self) -> Result<Vec<u8>> {
        let header_size = self.header.encoded_size();
        if self.objects.is_empty() {
            let mut header = self.header.clone();
            header.header_size = u32::try_from(header_size)
                .map_err(|_| Error::Write("BLF header exceeds the u32 size limit".into()))?;
            header.file_size = header_size as u64;
            header.uncompressed_file_size = header_size as u64;
            header.object_count = 0;
            if header.restore_point_offset.is_some() {
                header.restore_point_offset = Some(0);
            }
            let mut out = Vec::with_capacity(header_size);
            header.write_to(&mut out);
            return Ok(out);
        }
        let mut body = Vec::new();
        let mut uncompressed_total: u64 = 0;
        let mut buf: Vec<u8> = Vec::new();
        let mut restore_point_offset = None;
        let mut writing_restore_points = false;
        for obj in &self.objects {
            let is_restore_point = obj.object_type() == ObjectType::RestorepointContainer.to_raw();
            if is_restore_point && restore_point_offset.is_none() {
                if !buf.is_empty() {
                    uncompressed_total +=
                        flush_container(&mut body, &buf, self.header.compression > 0)?;
                    buf.clear();
                }
                restore_point_offset = Some((header_size + body.len()) as u64);
                writing_restore_points = true;
            } else if !is_restore_point && writing_restore_points {
                if !buf.is_empty() {
                    uncompressed_total +=
                        flush_container(&mut body, &buf, self.header.compression > 0)?;
                    buf.clear();
                }
                writing_restore_points = false;
            }
            let mut obj_bytes = Vec::new();
            obj.write_to(&mut obj_bytes)?;
            if !buf.is_empty() && buf.len() + obj_bytes.len() >= MAX_CONTAINER_BUFFER {
                uncompressed_total +=
                    flush_container(&mut body, &buf, self.header.compression > 0)?;
                buf.clear();
            }
            buf.extend_from_slice(&obj_bytes);
            let pad = obj.padding() as usize;
            buf.resize(buf.len() + pad, 0);
        }
        uncompressed_total += flush_container(&mut body, &buf, self.header.compression > 0)?;

        let mut header = self.header.clone();
        header.header_size = u32::try_from(header_size)
            .map_err(|_| Error::Write("BLF header exceeds the u32 size limit".into()))?;
        header.object_count = u32::try_from(self.objects.len()).unwrap_or(u32::MAX);
        header.file_size = (header_size + body.len()) as u64;
        header.uncompressed_file_size = header_size as u64 + uncompressed_total;
        if header.restore_point_offset.is_some() {
            header.restore_point_offset = Some(restore_point_offset.unwrap_or(0));
        }
        let mut out = Vec::with_capacity(header_size + body.len());
        header.write_to(&mut out);
        if out.len() != header_size {
            return Err(Error::Write(format!(
                "BLF header produced {} bytes but declared {header_size}",
                out.len()
            )));
        }
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::parse_reader(std::io::BufReader::new(file))
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let file = std::fs::File::create(path)?;
        self.write_to(std::io::BufWriter::new(file))?;
        Ok(())
    }

    /// Serializes this file into an arbitrary byte sink.
    pub fn write_to(&self, mut writer: impl Write) -> Result<()> {
        writer.write_all(&self.write()?)?;
        writer.flush()?;
        Ok(())
    }

    pub fn iter_objects<'a>(
        &'a self,
        filter: Option<&'a [ObjectType]>,
    ) -> impl Iterator<Item = &'a BlfObject> + 'a {
        self.objects.iter().filter(move |o| match filter {
            None => true,
            Some(f) => f.iter().any(|t| t.to_raw() == o.object_type()),
        })
    }

    pub fn statistics(&self) -> Vec<(u32, usize)> {
        let mut out: Vec<(u32, usize)> = Vec::new();
        for o in &self.objects {
            let t = o.object_type();
            match out.iter_mut().find(|(k, _)| *k == t) {
                Some((_, c)) => *c += 1,
                None => out.push((t, 1)),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::*;

    const NS: ObjectFlags = ObjectFlags::TIME_ONE_NANS;

    fn sample_objects() -> Vec<BlfObject> {
        let mut fd64 = CanFdManaged::new(NS, 6_000);
        fd64.channel = 1;
        fd64.dlc = 9;
        fd64.id = 0x321;
        fd64.dir = 1;
        fd64.fd_flags = CanFd64Flags::EDL | CanFd64Flags::BRS;
        fd64.data = vec![0x66; 12];
        fd64.valid_data_bytes = 12;
        fd64.header.base.object_size = 84;

        let mut stat = CanStatistic::new(NS, 7_000);
        stat.channel = 1;
        stat.bus_load = 25;
        stat.data_frames = 1000;

        let mut drv = CanDriverError::new(NS, 8_000);
        drv.channel = 2;
        drv.tx_errors = 1;

        let mut err_ext = CanErrorExt::new(NS, 4_000);
        err_ext.channel = 1;
        err_ext.ecc = EccFlags::STUFF_ERROR;
        err_ext.flags = CanErrorExtFlags::RX;

        let mut err = CanError::new(NS, 3_000);
        err.channel = 1;
        err.length = 6;

        vec![
            BlfObject::CanMessage(CanMessage::new(NS, 1_000, 1, 0x100, &[1, 2, 3, 4], true)),
            BlfObject::CanMessage2(CanMessage2::new(
                NS,
                2_000,
                2,
                0x200,
                &[5, 6, 7, 8, 9],
                false,
            )),
            BlfObject::CanError(err),
            BlfObject::CanErrorExt(err_ext),
            BlfObject::CanFdMessage(CanFdMessage::new(
                NS,
                5_000,
                1,
                0x300,
                &[0xAA; 16],
                true,
                true,
                true,
            )),
            BlfObject::CanFdMessage64(fd64),
            BlfObject::CanStatistic(stat),
            BlfObject::CanDriverError(drv),
            BlfObject::AppText(AppTextManaged::new(NS, 9_000, 3, "measurement start")),
            BlfObject::SysVariable(SysVariableManaged::new(
                NS,
                10_000,
                SysVarDataType::Long,
                0,
                "counter",
                42i32.to_le_bytes().to_vec(),
            )),
            BlfObject::RestorePointContainer(RestorePointContainerManaged::new(
                NS,
                11_000,
                vec![1, 2, 3],
            )),
        ]
    }

    #[test]
    fn roundtrip_compressed() {
        let mut blf = BlfFile::new(true, AppId::CANALYZER, 1, 2, 3);
        blf.objects = sample_objects();
        blf.header.start = Time {
            year: 2024,
            month: 1,
            day_of_week: 1,
            day: 15,
            hour: 10,
            minute: 0,
            second: 0,
            milliseconds: 0,
        };
        blf.header.end = Time {
            second: 11,
            ..blf.header.start
        };

        let bytes = blf.write().unwrap();
        assert_eq!(&bytes[0..4], b"LOGG");
        assert_eq!(
            u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
            11,
            "object_count"
        );
        assert_eq!(
            u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize,
            bytes.len(),
            "file_size"
        );
        assert!(
            u64::from_le_bytes(bytes[24..32].try_into().unwrap()) > 0,
            "uncompressed_file_size"
        );
        assert_eq!(&bytes[HEADER_SIZE..HEADER_SIZE + 4], b"LOBJ");
        assert_eq!(
            u32::from_le_bytes(
                bytes[HEADER_SIZE + 12..HEADER_SIZE + 16]
                    .try_into()
                    .unwrap()
            ),
            10
        );
        assert_eq!(bytes[HEADER_SIZE + 16], 2, "compression method");
        assert_eq!(&bytes[HEADER_SIZE + 32..HEADER_SIZE + 34], &[0x78, 0x01]);

        let back = BlfFile::parse(&bytes).unwrap();
        assert_eq!(back.header.app_id, AppId::CANALYZER);
        assert_eq!(back.header.compression, 1);
        assert_eq!(back.header.object_count, 11);
        assert_eq!(back.header.start, blf.header.start);
        assert_eq!(back.objects.len(), 11);
        match &back.objects[0] {
            BlfObject::CanMessage(m) => {
                assert_eq!(m.channel, 1);
                assert_eq!(m.id, 0x100);
                assert_eq!(m.dlc, 4);
                assert_eq!(m.flags, CanFlags::TX);
                assert_eq!(&m.data[..4], &[1, 2, 3, 4]);
                assert_eq!(m.header.timestamp, 1_000);
            }
            o => panic!("expected CanMessage, got {o:?}"),
        }
        match &back.objects[1] {
            BlfObject::CanMessage2(m) => {
                assert_eq!(m.frame_length, 5);
                assert_eq!(m.bit_count, 40);
                assert_eq!(m.flags, CanFlags::RX);
            }
            o => panic!("expected CanMessage2, got {o:?}"),
        }
        match &back.objects[3] {
            BlfObject::CanErrorExt(m) => {
                assert!(m.ecc.contains(EccFlags::STUFF_ERROR));
                assert_eq!(m.flags, CanErrorExtFlags::RX);
            }
            o => panic!("expected CanErrorExt, got {o:?}"),
        }
        match &back.objects[4] {
            BlfObject::CanFdMessage(m) => {
                assert_eq!(m.dlc, 10, "16 bytes -> DLC 10");
                assert!(m.fd_flags.contains(CanFdFlags::EDL));
                assert!(m.fd_flags.contains(CanFdFlags::BRS));
                assert_eq!(&m.data[..16], &[0xAA; 16]);
            }
            o => panic!("expected CanFdMessage, got {o:?}"),
        }
        match &back.objects[5] {
            BlfObject::CanFdMessage64(m) => {
                assert_eq!(m.data, vec![0x66; 12]);
                assert_eq!(m.id, 0x321);
                assert!(m.fd_flags.contains(CanFd64Flags::EDL));
            }
            o => panic!("expected CanFdMessage64, got {o:?}"),
        }
        match &back.objects[6] {
            BlfObject::CanStatistic(m) => {
                assert_eq!(m.bus_load, 25);
                assert_eq!(m.data_frames, 1000);
            }
            o => panic!("expected CanStatistic, got {o:?}"),
        }
        match &back.objects[7] {
            BlfObject::CanDriverError(m) => assert_eq!(m.tx_errors, 1),
            o => panic!("expected CanDriverError, got {o:?}"),
        }
        match &back.objects[8] {
            BlfObject::AppText(m) => {
                assert_eq!(m.text, "measurement start");
                assert_eq!(m.source, 3);
            }
            o => panic!("expected AppText, got {o:?}"),
        }
        match &back.objects[9] {
            BlfObject::SysVariable(m) => {
                assert_eq!(m.name, "counter");
                assert_eq!(m.data_type, SysVarDataType::Long);
                assert_eq!(m.data, 42i32.to_le_bytes().to_vec());
            }
            o => panic!("expected SysVariable, got {o:?}"),
        }
        match &back.objects[10] {
            BlfObject::RestorePointContainer(m) => assert_eq!(m.data, vec![1, 2, 3]),
            o => panic!("expected RestorePointContainer, got {o:?}"),
        }
        assert_eq!(back, blf_equivalent(&blf, &back));
    }

    fn blf_equivalent(expected: &BlfFile, actual: &BlfFile) -> BlfFile {
        let mut e = expected.clone();
        e.header.object_count = actual.header.object_count;
        e.header.file_size = actual.header.file_size;
        e.header.uncompressed_file_size = actual.header.uncompressed_file_size;
        e.header.restore_point_offset = actual.header.restore_point_offset;
        e.frame_time_base = actual.frame_time_base;
        e
    }

    #[test]
    fn roundtrip_uncompressed() {
        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = sample_objects();
        let bytes = blf.write().unwrap();
        assert_eq!(bytes[HEADER_SIZE + 16], 0, "compression method 0");
        assert!(u64::from_le_bytes(bytes[24..32].try_into().unwrap()) > HEADER_SIZE as u64);
        let back = BlfFile::parse(&bytes).unwrap();
        assert_eq!(back, blf_equivalent(&blf, &back));
    }

    #[test]
    fn uncompressed_size_matches_buffered_bytes() {
        let mut blf = BlfFile::new(true, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = vec![
            BlfObject::CanMessage(CanMessage::new(NS, 0, 1, 1, &[1], true)), // 48, pad 0
            BlfObject::AppText(AppTextManaged::new(NS, 0, 0, "abc")),        // 51, pad 51&3=3
        ];
        let bytes = blf.write().unwrap();
        let expected = (HEADER_SIZE + LOG_CONTAINER_SIZE + 48 + 51 + 3) as u64;
        assert_eq!(
            u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            expected
        );
    }

    #[test]
    fn restore_point_offset_targets_a_dedicated_container() {
        let mut blf = BlfFile::new(true, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = sample_objects();
        let bytes = blf.write().unwrap();
        let parsed = BlfFile::parse(&bytes).unwrap();
        let offset = parsed.header.restore_point_offset.unwrap() as usize;
        assert!(offset >= parsed.header.header_size as usize);
        let base = ObjBase::parse(&bytes[offset..], offset as u64).unwrap();
        assert_eq!(base.object_type(), Some(ObjectType::LogContainer));
    }

    #[test]
    fn empty_write_produces_valid_header() {
        let blf = BlfFile::new(true, AppId::UNKNOWN, 0, 0, 0);
        let bytes = blf.write().unwrap();
        assert_eq!(bytes.len(), HEADER_SIZE);
        assert_eq!(&bytes[..4], b"LOGG");
        let parsed = BlfFile::parse(&bytes).unwrap();
        assert!(parsed.objects.is_empty());
    }

    #[test]
    fn statistics_and_filter() {
        let mut blf = BlfFile::new(true, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = sample_objects();
        let mut header = LObj::new(ObjectType::CanMessage, NS, 0, 36);
        header.base.object_type = 11; // LIN_MESSAGE
        blf.objects.push(BlfObject::Raw {
            header,
            data: vec![0; 4],
        });

        let stats = blf.statistics();
        let total: usize = stats.iter().map(|(_, c)| c).sum();
        assert_eq!(total, 12);
        assert!(stats.contains(&(1, 1))); // CAN_MESSAGE
        assert!(stats.contains(&(11, 1)));

        let can_frames: Vec<_> = blf.iter_objects(Some(&BlfFile::CAN_DATA_FRAMES)).collect();
        assert_eq!(can_frames.len(), 4);
        let all: Vec<_> = blf.iter_objects(None).collect();
        assert_eq!(all.len(), 12);
        let fd_only: Vec<_> = blf
            .iter_objects(Some(&[ObjectType::CanFdMessage64]))
            .collect();
        assert_eq!(fd_only.len(), 1);
    }

    #[test]
    fn unknown_object_with_padding_preserved() {
        let mut header = LObj::new(ObjectType::CanMessage, NS, 12, 33);
        header.base.object_type = 90;
        let mut blf = BlfFile::new(true, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = vec![
            BlfObject::CanMessage(CanMessage::new(NS, 1, 1, 0x10, &[1], true)),
            BlfObject::Raw {
                header,
                data: vec![0xAB],
            },
            BlfObject::CanMessage(CanMessage::new(NS, 2, 1, 0x20, &[2], true)),
        ];
        let bytes = blf.write().unwrap();
        let back = BlfFile::parse(&bytes).unwrap();
        assert_eq!(back.objects.len(), 3);
        match &back.objects[1] {
            BlfObject::Raw { header, data } => {
                assert_eq!(header.base.object_type, 90);
                assert_eq!(header.timestamp, 12);
                assert_eq!(data, &vec![0xAB]);
            }
            o => panic!("expected Raw, got {o:?}"),
        }
        match &back.objects[2] {
            BlfObject::CanMessage(m) => assert_eq!(m.id, 0x20),
            o => panic!("expected CanMessage, got {o:?}"),
        }
    }

    #[test]
    fn parse_rejects_bad_files() {
        assert!(BlfFile::parse(&[]).is_err());
        assert!(BlfFile::parse(&[0u8; 100]).is_err());
        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = sample_objects();
        let mut bytes = blf.write().unwrap();
        bytes[HEADER_SIZE] = b'X';
        assert!(BlfFile::parse(&bytes).is_err());
    }

    #[test]
    fn container_rollover_at_4mib() {
        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        let msg = CanMessage::new(NS, 1, 1, 0x100, &[1, 2, 3, 4], true);
        blf.objects = (0..90_000)
            .map(|_| BlfObject::CanMessage(msg.clone()))
            .collect();
        let bytes = blf.write().unwrap();
        let mut pos = HEADER_SIZE;
        let mut containers = 0;
        while pos < bytes.len() {
            let size = u32::from_le_bytes(bytes[pos + 8..pos + 12].try_into().unwrap()) as usize;
            let otype = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().unwrap());
            assert_eq!(otype, 10, "top-level object must be LOG_CONTAINER");
            containers += 1;
            pos += size + (size & 3);
        }
        assert_eq!(containers, 2);
        let back = BlfFile::parse(&bytes).unwrap();
        assert_eq!(back.objects.len(), 90_000);
        assert_eq!(back.header.object_count, 90_000);
    }

    #[test]
    fn open_save_file_io() {
        let mut blf = BlfFile::new(true, AppId::CANAPE, 5, 0, 1);
        blf.objects = sample_objects();
        let dir = std::env::temp_dir().join(format!("autors_blf_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.blf");
        blf.save(&path).unwrap();
        let back = BlfFile::open(&path).unwrap();
        assert_eq!(back, blf_equivalent(&blf, &back));
        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn zlib_helpers() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let packed = zlib_deflate(&data).unwrap();
        assert_eq!(&packed[..2], &[0x78, 0x01]);
        assert!(packed.len() < data.len(), "compressible data should shrink");
        assert_eq!(zlib_inflate_limited(&packed, data.len()).unwrap(), data);
        let n = packed.len();
        assert_eq!(
            u32::from_be_bytes(packed[n - 4..n].try_into().unwrap()),
            adler32(&data)
        );
        assert!(zlib_inflate_limited(&[0x78], 1024).is_err());
    }

    use autors_can::frame::FrameType;
    use std::time::Duration;

    fn frame(
        id: u32,
        data: &[u8],
        is_master: bool,
        frame_type: FrameType,
        elapsed_ms: u64,
    ) -> CanFrame {
        let mut f = CanFrame::new("TESTBUS", id, data.to_vec(), is_master, frame_type);
        f.elapsed = Duration::from_millis(elapsed_ms);
        f
    }

    fn assert_frame_eq(back: &CanFrame, expected: &CanFrame, channel: u16) {
        assert_eq!(
            back.bus_id,
            channel.to_string(),
            "bus_id = Channel.ToString()"
        );
        assert_eq!(back.id, expected.id, "ID including the extended-frame flag");
        assert_eq!(back.is_extended_id(), expected.is_extended_id());
        assert_eq!(back.raw_id(), expected.raw_id());
        assert_eq!(back.data, expected.data, "data");
        assert_eq!(back.is_master_frame, expected.is_master_frame);
        assert_eq!(back.frame_type, expected.frame_type, "frame_type");
    }

    #[test]
    fn can_frame_write_read_roundtrip() {
        let mut blf = BlfFile::new(true, AppId::CANOE, 1, 0, 0);
        let frames = [
            frame(0x100, &[1, 2, 3, 4], true, FrameType::CAN20B, 1_000),
            frame(
                0x8000_0123,
                &[5, 6, 7, 8, 9, 10, 11, 12],
                false,
                FrameType::CAN20B,
                1_500,
            ),
            frame(0x321, &[0xAA; 12], true, FrameType::FD, 2_000),
            frame(0x8000_0322, &[0x55; 16], false, FrameType::FD_BRS, 2_500),
        ];
        for f in &frames {
            blf.add_can_frame(f, 1, NS).unwrap();
        }
        assert_eq!(blf.objects.len(), 4);
        assert!(matches!(blf.objects[0], BlfObject::CanMessage(_)));
        assert!(matches!(blf.objects[1], BlfObject::CanMessage(_)));
        assert!(matches!(blf.objects[2], BlfObject::CanFdMessage(_)));
        assert!(matches!(blf.objects[3], BlfObject::CanFdMessage(_)));
        assert_eq!(blf.objects[0].header().timestamp, 0);
        assert_eq!(blf.objects[1].header().timestamp, 500_000_000);
        assert_eq!(blf.objects[2].header().timestamp, 1_000_000_000);
        match &blf.objects[2] {
            BlfObject::CanFdMessage(m) => {
                assert_eq!(m.dlc, 9, "12 bytes -> DLC 9");
                assert_eq!(m.fd_flags, CanFdFlags::EDL);
                assert_eq!(m.valid_data_bytes, 12);
            }
            o => panic!("expected CanFdMessage, got {o:?}"),
        }
        match &blf.objects[3] {
            BlfObject::CanFdMessage(m) => {
                assert_eq!(m.dlc, 10, "16 bytes -> DLC 10");
                assert!(m.fd_flags.contains(CanFdFlags::EDL | CanFdFlags::BRS));
            }
            o => panic!("expected CanFdMessage, got {o:?}"),
        }

        let bytes = blf.write().unwrap();
        let back = BlfFile::parse(&bytes).unwrap();
        let read = back.enum_can_frames(-1, u32::MAX);
        assert_eq!(read.len(), 4);
        for (b, e) in read.iter().zip(frames.iter()) {
            assert_frame_eq(b, e, 1);
        }
    }

    #[test]
    fn lin_frame_write_read_roundtrip_and_filtering() {
        let mut first = LinFrame::new("LIN", 0x12, vec![1, 2, 3], true);
        first.elapsed = Duration::from_millis(100);
        let mut second = LinFrame::new("LIN", 0x22, vec![4, 5], false);
        second.elapsed = Duration::from_millis(125);

        let mut blf = BlfFile::new(true, AppId::CANOE, 1, 0, 0);
        blf.add_lin_frame(&first, 1, LinChecksumType::CalcChecksumEnhanced, NS)
            .unwrap();
        blf.add_lin_frame(&second, 2, LinChecksumType::CalcChecksum, NS)
            .unwrap();
        assert!(matches!(blf.objects[0], BlfObject::LinMessage2(_)));
        assert_eq!(blf.objects[1].timestamp(), 25_000_000);

        let bytes = blf.write().unwrap();
        let back = BlfFile::parse(&bytes).unwrap();
        assert_eq!(back.enum_lin_frames(-1, u8::MAX).len(), 2);
        assert_eq!(back.enum_lin_frames(1, u8::MAX).len(), 1);
        assert_eq!(back.enum_lin_frames(-1, 0x22).len(), 1);
        let frames = back.enum_lin_frames(-1, u8::MAX);
        assert_eq!(frames[0].bus_id, "1");
        assert_eq!(frames[0].id, first.id);
        assert_eq!(frames[0].data, first.data);
        assert!(frames[0].is_master_frame);
        assert_eq!(frames[0].elapsed, Duration::ZERO);
        assert_eq!(frames[1].elapsed, Duration::from_millis(25));
        assert!(!frames[1].is_master_frame);
    }

    #[test]
    fn enum_lin_frames_decodes_legacy_message() {
        let mut blf = BlfFile::new(false, AppId::CANOE, 1, 0, 0);
        blf.objects.push(BlfObject::LinMessage(LinMessage::new(
            NS,
            1_000_000,
            4,
            0x3C,
            &[0xAA, 0x55],
            0,
            false,
        )));
        let frames = blf.enum_lin_frames(4, 0x3C);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, [0xAA, 0x55]);
        assert_eq!(frames[0].elapsed, Duration::from_millis(1));
    }

    #[test]
    fn add_lin_frame_rejects_invalid_id_and_length() {
        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        let invalid_id = LinFrame::new("LIN", 0x40, vec![1], false);
        assert!(blf
            .add_lin_frame(&invalid_id, 1, LinChecksumType::CalcChecksum, NS)
            .is_err());
        let invalid_length = LinFrame::new("LIN", 1, vec![0; 9], false);
        assert!(blf
            .add_lin_frame(&invalid_length, 1, LinChecksumType::CalcChecksum, NS)
            .is_err());
    }

    #[test]
    fn add_can_frame_timestamp_units() {
        // TimeOneNans:ms × 1e6
        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        blf.add_can_frame(&frame(1, &[1], true, FrameType::CAN20B, 1_000), 1, NS)
            .unwrap();
        blf.add_can_frame(&frame(2, &[2], true, FrameType::CAN20B, 1_500), 1, NS)
            .unwrap();
        assert_eq!(
            blf.objects[0].header().timestamp,
            0,
            "the first frame always starts at zero"
        );
        assert_eq!(blf.objects[1].header().timestamp, 500_000_000);

        // TimeTenMics:ms × 100
        let mut blf10 = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        blf10
            .add_can_frame(
                &frame(1, &[1], true, FrameType::CAN20B, 1_000),
                1,
                ObjectFlags::TIME_TEN_MICS,
            )
            .unwrap();
        blf10
            .add_can_frame(
                &frame(2, &[2], true, FrameType::CAN20B, 1_001),
                1,
                ObjectFlags::TIME_TEN_MICS,
            )
            .unwrap();
        assert_eq!(blf10.objects[1].header().timestamp, 100, "1ms = 100 × 10µs");

        assert!(blf
            .add_can_frame(&frame(3, &[3], true, FrameType::CAN20B, 500), 1, NS)
            .is_err());
    }

    #[test]
    fn add_can_frame_rejects_oversized_data() {
        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        assert!(blf
            .add_can_frame(&frame(1, &[0; 9], true, FrameType::CAN20B, 0), 1, NS)
            .is_err());
        assert!(blf
            .add_can_frame(&frame(1, &[0; 65], true, FrameType::FD, 0), 1, NS)
            .is_err());
        blf.add_can_frame(&frame(1, &[0; 8], true, FrameType::CAN20B, 0), 1, NS)
            .unwrap();
        blf.add_can_frame(&frame(1, &[0; 64], true, FrameType::FD, 0), 1, NS)
            .unwrap();
        assert_eq!(blf.objects.len(), 2);
    }

    #[test]
    fn enum_can_frames_channel_id_filter() {
        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = vec![
            BlfObject::CanMessage(CanMessage::new(NS, 0, 1, 0x100, &[1; 4], true)),
            BlfObject::CanMessage(CanMessage::new(NS, 0, 2, 0x100, &[2; 4], false)),
            BlfObject::CanMessage(CanMessage::new(NS, 0, 1, 0x200, &[3; 4], true)),
        ];
        assert_eq!(blf.enum_can_frames(-1, u32::MAX).len(), 3);
        assert_eq!(blf.enum_can_frames(1, u32::MAX).len(), 2);
        assert_eq!(blf.enum_can_frames(-1, 0x100).len(), 2);
        assert_eq!(blf.enum_can_frames(2, 0x100).len(), 1);
        assert_eq!(blf.enum_can_frames(2, 0x200).len(), 0);
        let f = &blf.enum_can_frames(2, 0x100)[0];
        assert_eq!(f.bus_id, "2");
        assert!(!f.is_master_frame);
    }

    #[test]
    fn enum_can_frames_skips_rtr_empty_and_error_flags() {
        let mut rtr = CanMessage::new(NS, 0, 1, 0x300, &[4; 4], true);
        rtr.flags = CanFlags::RTR;
        let mut tx_rtr = CanMessage::new(NS, 0, 1, 0x301, &[5; 4], true);
        tx_rtr.flags = CanFlags::TX | CanFlags::RTR;
        let mut nerr = CanMessage::new(NS, 0, 1, 0x302, &[6; 4], true);
        nerr.flags = CanFlags::TX | CanFlags::NERR;
        let empty = CanMessage::new(NS, 0, 1, 0x303, &[], true); // DLC 0
        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = vec![
            BlfObject::CanMessage(CanMessage::new(NS, 0, 1, 0x100, &[1; 4], true)),
            BlfObject::CanMessage(rtr),
            BlfObject::CanMessage(tx_rtr),
            BlfObject::CanMessage(nerr),
            BlfObject::CanMessage(empty),
        ];
        let frames = blf.enum_can_frames(-1, u32::MAX);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id, 0x100);
    }

    #[test]
    fn enum_fd64_frame_type_uses_cs_bswap_quirk() {
        let mut std_fd = CanFdManaged::new(NS, 0);
        std_fd.channel = 3;
        std_fd.id = 0x500;
        std_fd.dir = 1; // Dir > 0 → is_master_frame
        std_fd.data = vec![9; 8];
        std_fd.valid_data_bytes = 8;
        std_fd.fd_flags = CanFd64Flags::EDL | CanFd64Flags::BRS;
        let mut crafted = std_fd.clone();
        crafted.id = 0x501;
        crafted.dir = 0;
        crafted.fd_flags = CanFd64Flags::from_bits(0x0010_0000);
        let mut empty = std_fd.clone();
        empty.id = 0x502;
        empty.data = Vec::new();
        empty.valid_data_bytes = 0;

        let mut blf = BlfFile::new(false, AppId::UNKNOWN, 0, 0, 0);
        blf.objects = vec![
            BlfObject::CanFdMessage64(std_fd),
            BlfObject::CanFdMessage64(crafted),
            BlfObject::CanFdMessage64(empty),
        ];
        let frames = blf.enum_can_frames(-1, u32::MAX);
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[0].frame_type,
            FrameType::CAN20B,
            "byte-swapping a standard EDL frame yields zero"
        );
        assert_eq!(frames[0].bus_id, "3");
        assert!(frames[0].is_master_frame, "Dir=1 → TX");
        assert_eq!(frames[0].data, vec![9; 8]);
        assert_eq!(frames[1].frame_type, FrameType::FD);
        assert!(!frames[1].is_master_frame, "Dir=0 → RX");
    }
}
