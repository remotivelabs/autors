//! Streaming BLF input and output.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::{Error, Result};
use crate::file::{encode_container, zlib_inflate_limited};
use crate::objects::{
    BlfObject, Header, LogContainer, ObjBase, ObjectType, FILE_SIGNATURE, HEADER_CORE_SIZE,
    LOG_CONTAINER_SIZE, OBJ_BASE_SIZE, OBJ_SIGNATURE,
};

const DEFAULT_CONTAINER_SIZE: usize = 4 * 1024 * 1024;

/// Resource limits applied while reading untrusted BLF input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlfReaderOptions {
    pub max_header_size: usize,
    pub max_container_size: usize,
    pub max_object_size: usize,
}

impl Default for BlfReaderOptions {
    fn default() -> Self {
        Self {
            max_header_size: 16 * 1024 * 1024,
            max_container_size: 512 * 1024 * 1024,
            max_object_size: 256 * 1024 * 1024,
        }
    }
}

fn read_exact_at<R: Read>(reader: &mut R, buffer: &mut [u8], offset: u64) -> Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => {
                return Err(Error::Parse {
                    offset: offset + filled as u64,
                    message: format!(
                        "unexpected end of file: need {} more bytes",
                        buffer.len() - filled
                    ),
                });
            }
            Ok(count) => filled += count,
            Err(error) => return Err(Error::Io(error)),
        }
    }
    Ok(())
}

/// Incremental BLF reader over any byte stream.
pub struct BlfReader<R> {
    inner: R,
    header: Header,
    object_data: Vec<u8>,
    object_pos: usize,
    file_offset: u64,
    source_eof: bool,
    finished: bool,
    options: BlfReaderOptions,
}

impl<R: Read> BlfReader<R> {
    /// Reads the file header and prepares an object iterator.
    pub fn new(inner: R) -> Result<Self> {
        Self::with_options(inner, BlfReaderOptions::default())
    }

    /// Reads a file using explicit limits for allocations and decompression.
    pub fn with_options(mut inner: R, options: BlfReaderOptions) -> Result<Self> {
        if options.max_header_size < HEADER_CORE_SIZE
            || options.max_container_size < LOG_CONTAINER_SIZE
            || options.max_object_size < OBJ_BASE_SIZE
        {
            return Err(Error::Parse {
                offset: 0,
                message: "BLF reader limits are smaller than mandatory format headers".into(),
            });
        }
        let mut prefix = [0u8; 8];
        read_exact_at(&mut inner, &mut prefix, 0)?;
        if prefix[..4] != FILE_SIGNATURE {
            return Err(Error::Parse {
                offset: 0,
                message: "bad BLF file signature".into(),
            });
        }
        let header_size = u32::from_le_bytes(prefix[4..8].try_into().map_err(|_| Error::Parse {
            offset: 4,
            message: "invalid BLF header-size field".into(),
        })?) as usize;
        if !(HEADER_CORE_SIZE..=options.max_header_size).contains(&header_size) {
            return Err(Error::Parse {
                offset: 4,
                message: format!(
                    "BLF header size {header_size} is outside {HEADER_CORE_SIZE}..={} ",
                    options.max_header_size
                ),
            });
        }
        let mut header_bytes = vec![0u8; header_size];
        header_bytes[..8].copy_from_slice(&prefix);
        read_exact_at(&mut inner, &mut header_bytes[8..], 8)?;
        let header = Header::parse(&header_bytes, 0)?;
        Ok(Self {
            inner,
            header,
            object_data: Vec::new(),
            object_pos: 0,
            file_offset: header_size as u64,
            source_eof: false,
            finished: false,
            options,
        })
    }

    /// Returns the parsed BLF file header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Consumes the reader and returns the underlying stream.
    pub fn into_inner(self) -> R {
        self.inner
    }

    fn compact_object_data(&mut self) {
        if self.object_pos == 0 {
            return;
        }
        if self.object_pos == self.object_data.len() {
            self.object_data.clear();
        } else {
            self.object_data.copy_within(self.object_pos.., 0);
            self.object_data
                .truncate(self.object_data.len() - self.object_pos);
        }
        self.object_pos = 0;
    }

    fn try_object(&mut self) -> Result<Option<BlfObject>> {
        let remaining = &self.object_data[self.object_pos..];
        if remaining.is_empty() {
            return Ok(None);
        }
        if remaining.iter().all(|byte| *byte == 0) {
            self.object_pos = self.object_data.len();
            return Ok(None);
        }
        if remaining.len() < OBJ_BASE_SIZE {
            return Ok(None);
        }
        let base = ObjBase::parse(remaining, self.object_pos as u64)?;
        if base.signature != OBJ_SIGNATURE {
            return Err(Error::Parse {
                offset: self.object_pos as u64,
                message: "bad object signature in uncompressed stream".into(),
            });
        }
        let object_size = base.object_size as usize;
        if object_size < OBJ_BASE_SIZE {
            return Err(Error::Parse {
                offset: self.object_pos as u64 + 8,
                message: format!("object size {object_size} is smaller than its base header"),
            });
        }
        if object_size > self.options.max_object_size {
            return Err(Error::Parse {
                offset: self.object_pos as u64 + 8,
                message: format!(
                    "object size {object_size} exceeds configured limit {}",
                    self.options.max_object_size
                ),
            });
        }
        if remaining.len() < object_size {
            return Ok(None);
        }
        let object = BlfObject::parse(&remaining[..object_size], self.object_pos as u64)?;
        let consumed = object_size + object.padding() as usize;
        if remaining.len() < consumed {
            return Ok(None);
        }
        self.object_pos += consumed;
        Ok(Some(object))
    }

    fn read_top_level(&mut self) -> Result<Option<BlfObject>> {
        let mut base_bytes = [0u8; OBJ_BASE_SIZE];
        let first = match self.inner.read(&mut base_bytes[..1]) {
            Ok(0) => {
                self.source_eof = true;
                return Ok(None);
            }
            Ok(_) => 1,
            Err(error) => return Err(Error::Io(error)),
        };
        read_exact_at(
            &mut self.inner,
            &mut base_bytes[first..],
            self.file_offset + first as u64,
        )?;
        let base = ObjBase::parse(&base_bytes, self.file_offset)?;
        if base.signature != OBJ_SIGNATURE {
            return Err(Error::Parse {
                offset: self.file_offset,
                message: "bad top-level object signature".into(),
            });
        }
        let object_size = base.object_size as usize;
        if object_size < OBJ_BASE_SIZE {
            return Err(Error::Parse {
                offset: self.file_offset + 8,
                message: format!("top-level object size {object_size} is too small"),
            });
        }
        let size_limit = if base.object_type() == Some(ObjectType::LogContainer) {
            self.options.max_container_size
        } else {
            self.options.max_object_size
        };
        if object_size > size_limit {
            return Err(Error::Parse {
                offset: self.file_offset + 8,
                message: format!(
                    "top-level object size {object_size} exceeds configured limit {size_limit}"
                ),
            });
        }
        let mut bytes = vec![0u8; object_size];
        bytes[..OBJ_BASE_SIZE].copy_from_slice(&base_bytes);
        read_exact_at(
            &mut self.inner,
            &mut bytes[OBJ_BASE_SIZE..],
            self.file_offset + OBJ_BASE_SIZE as u64,
        )?;

        if base.object_type() == Some(ObjectType::LogContainer) {
            if object_size < LOG_CONTAINER_SIZE {
                return Err(Error::Parse {
                    offset: self.file_offset,
                    message: "LOG_CONTAINER is smaller than its fixed header".into(),
                });
            }
            let container = LogContainer::parse(&bytes, self.file_offset)?;
            let payload = &bytes[LOG_CONTAINER_SIZE..];
            let data = match container.compression_method {
                0 => {
                    if container.uncompressed_size != 0
                        && container.uncompressed_size as usize != payload.len()
                    {
                        return Err(Error::Parse {
                            offset: self.file_offset,
                            message: format!(
                                "LOG_CONTAINER declares {} bytes but stores {}",
                                container.uncompressed_size,
                                payload.len()
                            ),
                        });
                    }
                    payload.to_vec()
                }
                2 => {
                    let declared = container.uncompressed_size as usize;
                    if declared > self.options.max_container_size {
                        return Err(Error::Parse {
                            offset: self.file_offset + 24,
                            message: format!(
                                "LOG_CONTAINER uncompressed size {declared} exceeds configured limit {}",
                                self.options.max_container_size
                            ),
                        });
                    }
                    let data = zlib_inflate_limited(payload, declared)?;
                    if data.len() != container.uncompressed_size as usize {
                        return Err(Error::Parse {
                            offset: self.file_offset,
                            message: format!(
                                "LOG_CONTAINER declares {} bytes but expands to {}",
                                container.uncompressed_size,
                                data.len()
                            ),
                        });
                    }
                    data
                }
                method => {
                    return Err(Error::Parse {
                        offset: self.file_offset + 16,
                        message: format!("unsupported LOG_CONTAINER compression method {method}"),
                    });
                }
            };
            let padding = base.object_size & 3;
            let mut padding_bytes = [0u8; 3];
            read_exact_at(
                &mut self.inner,
                &mut padding_bytes[..padding as usize],
                self.file_offset + object_size as u64,
            )?;
            self.file_offset += object_size as u64 + u64::from(padding);
            self.compact_object_data();
            self.object_data.extend_from_slice(&data);
            Ok(None)
        } else {
            let object = BlfObject::parse(&bytes, self.file_offset)?;
            let padding = object.padding();
            let mut padding_bytes = [0u8; 3];
            read_exact_at(
                &mut self.inner,
                &mut padding_bytes[..padding as usize],
                self.file_offset + object_size as u64,
            )?;
            self.file_offset += object_size as u64 + u64::from(padding);
            Ok(Some(object))
        }
    }
}

impl<R: Read> Iterator for BlfReader<R> {
    type Item = Result<BlfObject>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            match self.try_object() {
                Ok(Some(object)) => return Some(Ok(object)),
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
                Ok(None) => {}
            }
            match self.read_top_level() {
                Ok(Some(object)) => {
                    if self.object_data[self.object_pos..]
                        .iter()
                        .any(|byte| *byte != 0)
                    {
                        self.finished = true;
                        return Some(Err(Error::Parse {
                            offset: self.object_pos as u64,
                            message: "incomplete inner object before top-level object".into(),
                        }));
                    }
                    return Some(Ok(object));
                }
                Ok(None) if !self.source_eof => continue,
                Ok(None)
                    if self.object_data[self.object_pos..]
                        .iter()
                        .all(|byte| *byte == 0) =>
                {
                    self.finished = true;
                    return None;
                }
                Ok(None) => {
                    self.finished = true;
                    return Some(Err(Error::Parse {
                        offset: self.object_pos as u64,
                        message: "truncated object at end of uncompressed stream".into(),
                    }));
                }
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

/// Incremental BLF writer for seekable byte streams.
pub struct BlfWriter<W> {
    inner: W,
    header: Header,
    header_offset: u64,
    buffer: Vec<u8>,
    container_size: usize,
    uncompressed_size: u64,
    object_count: u64,
    restore_point_offset: Option<u64>,
}

impl<W: Write + Seek> BlfWriter<W> {
    /// Writes a placeholder header at the stream's current position.
    pub fn new(mut inner: W, mut header: Header) -> Result<Self> {
        let header_offset = inner.stream_position()?;
        let header_size = header.encoded_size();
        header.header_size = u32::try_from(header_size)
            .map_err(|_| Error::Write("BLF header exceeds the u32 size limit".into()))?;
        header.file_size = header_size as u64;
        header.uncompressed_file_size = header_size as u64;
        header.object_count = 0;
        let mut bytes = Vec::with_capacity(header_size);
        header.write_to(&mut bytes);
        if bytes.len() != header_size {
            return Err(Error::Write(
                "BLF header size does not match its fields".into(),
            ));
        }
        inner.write_all(&bytes)?;
        Ok(Self {
            inner,
            header,
            header_offset,
            buffer: Vec::new(),
            container_size: DEFAULT_CONTAINER_SIZE,
            uncompressed_size: header_size as u64,
            object_count: 0,
            restore_point_offset: None,
        })
    }

    /// Sets the maximum uncompressed bytes buffered per log container.
    pub fn set_container_size(&mut self, bytes: usize) -> Result<()> {
        if bytes == 0 {
            return Err(Error::Write(
                "container size must be greater than zero".into(),
            ));
        }
        self.container_size = bytes;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let (container, uncompressed_size) =
            encode_container(&self.buffer, self.header.compression > 0)?;
        self.inner.write_all(&container)?;
        self.uncompressed_size += uncompressed_size;
        self.buffer.clear();
        Ok(())
    }

    /// Serializes one object, flushing a full log container when needed.
    pub fn write_object(&mut self, object: &BlfObject) -> Result<()> {
        if object.object_type() == ObjectType::RestorepointContainer.to_raw()
            && self.restore_point_offset.is_none()
        {
            self.flush()?;
            self.restore_point_offset = Some(
                self.inner
                    .stream_position()?
                    .saturating_sub(self.header_offset),
            );
        }
        let mut bytes = Vec::new();
        object.write_to(&mut bytes)?;
        let padding = object.padding() as usize;
        bytes.resize(bytes.len() + padding, 0);
        if !self.buffer.is_empty() && self.buffer.len() + bytes.len() >= self.container_size {
            self.flush()?;
        }
        self.buffer.extend_from_slice(&bytes);
        self.object_count += 1;
        Ok(())
    }

    /// Flushes pending data, updates the header, and returns the stream.
    pub fn finish(mut self) -> Result<W> {
        self.flush()?;
        let end = self.inner.stream_position()?;
        self.header.file_size = end.saturating_sub(self.header_offset);
        self.header.uncompressed_file_size = self.uncompressed_size;
        self.header.object_count = u32::try_from(self.object_count).unwrap_or(u32::MAX);
        if self.header.restore_point_offset.is_some() {
            self.header.restore_point_offset = Some(self.restore_point_offset.unwrap_or(0));
        }
        self.inner.seek(SeekFrom::Start(self.header_offset))?;
        let mut bytes = Vec::with_capacity(self.header.encoded_size());
        self.header.write_to(&mut bytes);
        self.inner.write_all(&bytes)?;
        self.inner.seek(SeekFrom::Start(end))?;
        Ok(self.inner)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::objects::{AppId, CanMessage, ObjectFlags};

    #[test]
    fn streaming_writer_and_reader_roundtrip() {
        let header = Header::new(true, AppId::CANOE, 1, 0, 7);
        let mut writer = BlfWriter::new(Cursor::new(Vec::new()), header).unwrap();
        writer.set_container_size(64).unwrap();
        let first = BlfObject::CanMessage(CanMessage::new(
            ObjectFlags::TIME_ONE_NANS,
            10,
            1,
            0x123,
            &[1, 2, 3],
            true,
        ));
        let second = BlfObject::CanMessage(CanMessage::new(
            ObjectFlags::TIME_ONE_NANS,
            20,
            2,
            0x456,
            &[4, 5],
            false,
        ));
        writer.write_object(&first).unwrap();
        writer.write_object(&second).unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let mut reader = BlfReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.header().object_count, 2);
        assert_eq!(reader.next().unwrap().unwrap(), first);
        assert_eq!(reader.next().unwrap().unwrap(), second);
        assert!(reader.next().is_none());
    }

    #[test]
    fn reader_limits_reject_oversized_header_before_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&FILE_SIGNATURE);
        bytes.extend_from_slice(&1024u32.to_le_bytes());
        let options = BlfReaderOptions {
            max_header_size: HEADER_CORE_SIZE,
            ..BlfReaderOptions::default()
        };
        assert!(BlfReader::with_options(Cursor::new(bytes), options).is_err());
    }
}
