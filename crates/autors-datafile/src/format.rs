//! Content-first format detection and automatic dispatch.

use std::path::Path;

use crate::datafile::{DataFile, DataFileType, MemorySegmentList};
use crate::uf2::has_uf2_magic;
use crate::{Error, Result};

/// Formats recognized by the detector. Some OEM containers are recognized so
/// callers receive an explicit unsupported-format error instead of silently
/// treating structured input as a raw binary image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataFormat {
    Binary,
    IntelHex,
    MotorolaS,
    MixedHexSRecord,
    HexAscii,
    FordVbf,
    FordIHex,
    TiTxt,
    Uf2,
    GmBinary,
    FiatPrm,
    GacBinary,
    CheryCbf,
}

impl DataFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Binary => "raw binary",
            Self::IntelHex => "Intel HEX",
            Self::MotorolaS => "Motorola S-record",
            Self::MixedHexSRecord => "mixed Intel HEX/Motorola S-record",
            Self::HexAscii => "hexadecimal ASCII",
            Self::FordVbf => "Ford/Volvo VBF",
            Self::FordIHex => "Ford Intel HEX container",
            Self::TiTxt => "Texas Instruments TI-TXT",
            Self::Uf2 => "Microsoft UF2",
            Self::GmBinary => "GM binary container",
            Self::FiatPrm => "Fiat PRM/BIN container",
            Self::GacBinary => "GAC binary container",
            Self::CheryCbf => "Chery CBF container",
        }
    }

    pub fn is_supported(self) -> bool {
        self.data_file_type().is_some()
    }

    fn data_file_type(self) -> Option<DataFileType> {
        match self {
            Self::Binary => Some(DataFileType::Binary),
            Self::IntelHex => Some(DataFileType::IntelHex),
            Self::MotorolaS => Some(DataFileType::MotorolaS),
            Self::MixedHexSRecord => Some(DataFileType::Mixed),
            Self::HexAscii => Some(DataFileType::HexAscii),
            Self::FordVbf => Some(DataFileType::FordVbf),
            Self::FordIHex => Some(DataFileType::FordIHex),
            Self::TiTxt => Some(DataFileType::TiTxt),
            Self::Uf2 => Some(DataFileType::Uf2),
            Self::GmBinary | Self::FiatPrm | Self::GacBinary | Self::CheryCbf => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DetectionConfidence {
    Fallback,
    Extension,
    Likely,
    Certain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatDetection {
    pub format: DataFormat,
    pub confidence: DetectionConfidence,
    pub reason: &'static str,
}

impl FormatDetection {
    fn new(format: DataFormat, confidence: DetectionConfidence, reason: &'static str) -> Self {
        Self {
            format,
            confidence,
            reason,
        }
    }
}

fn extension(path: Option<&Path>) -> String {
    path.and_then(Path::extension)
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_binary(content: &[u8]) -> bool {
    if std::str::from_utf8(content).is_err() {
        return true;
    }
    content
        .iter()
        .take(4096)
        .any(|byte| matches!(byte, 0..=8 | 11 | 12 | 14..=31))
}

fn is_hex_payload(value: &str) -> bool {
    !value.is_empty()
        && value.len().is_multiple_of(2)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_intel_hex_line(line: &str) -> bool {
    line.strip_prefix(':')
        .is_some_and(|payload| payload.len() >= 10 && is_hex_payload(payload))
}

fn is_s_record_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 10
        && bytes[0].eq_ignore_ascii_case(&b'S')
        && bytes[1].is_ascii_digit()
        && is_hex_payload(&line[2..])
}

fn record_detection(text: &str) -> Option<FormatDetection> {
    let mut intel_hex = 0usize;
    let mut motorola_s = 0usize;
    let mut structured = 0usize;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(25)
    {
        if is_intel_hex_line(line) {
            intel_hex += 1;
            structured += 1;
        } else if is_s_record_line(line) {
            motorola_s += 1;
            structured += 1;
        }
    }
    match (intel_hex, motorola_s) {
        (0, 0) => None,
        (0, _) => Some(FormatDetection::new(
            DataFormat::MotorolaS,
            if structured >= 2 {
                DetectionConfidence::Certain
            } else {
                DetectionConfidence::Likely
            },
            "valid Motorola S-record lines",
        )),
        (_, 0) => Some(FormatDetection::new(
            DataFormat::IntelHex,
            if structured >= 2 {
                DetectionConfidence::Certain
            } else {
                DetectionConfidence::Likely
            },
            "valid Intel HEX record lines",
        )),
        _ => Some(FormatDetection::new(
            DataFormat::MixedHexSRecord,
            DetectionConfidence::Certain,
            "both Intel HEX and Motorola S-record lines",
        )),
    }
}

fn is_ti_txt(text: &str) -> bool {
    let mut has_address = false;
    let mut has_data = false;
    let mut has_terminator = false;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.eq_ignore_ascii_case("q") {
            has_terminator = true;
            break;
        }
        if let Some(address) = line.strip_prefix('@') {
            if address.is_empty() || !address.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return false;
            }
            has_address = true;
            continue;
        }
        if line
            .split_whitespace()
            .all(|token| token.len() == 2 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            has_data = true;
        } else {
            return false;
        }
    }
    has_address && has_data && has_terminator
}

/// Detects a format using signatures and record syntax before considering the
/// filename extension.
pub fn detect_format(path: Option<&Path>, content: &[u8]) -> FormatDetection {
    let extension = extension(path);
    let prefix_len = content.len().min(64 * 1024);
    let text_prefix = String::from_utf8_lossy(&content[..prefix_len]);
    let lower_prefix = text_prefix.to_ascii_lowercase();
    let full_text = std::str::from_utf8(content).ok();

    if has_uf2_magic(content) {
        return FormatDetection::new(
            DataFormat::Uf2,
            DetectionConfidence::Certain,
            "UF2 block magic",
        );
    }

    if lower_prefix.contains("vbf_version") {
        return FormatDetection::new(
            DataFormat::FordVbf,
            DetectionConfidence::Certain,
            "VBF version declaration",
        );
    }
    if lower_prefix.contains("application>ford") && text_prefix.lines().any(is_intel_hex_line) {
        return FormatDetection::new(
            DataFormat::FordIHex,
            DetectionConfidence::Certain,
            "Ford header followed by Intel HEX records",
        );
    }
    if full_text.is_some_and(is_ti_txt) {
        return FormatDetection::new(
            DataFormat::TiTxt,
            DetectionConfidence::Certain,
            "TI-TXT address records and q terminator",
        );
    }
    if let Some(detection) = record_detection(&text_prefix) {
        return detection;
    }
    match extension.as_str() {
        "prm" => {
            return FormatDetection::new(
                DataFormat::FiatPrm,
                DetectionConfidence::Extension,
                "Fiat parameter-file extension",
            );
        }
        "gbf" => {
            return FormatDetection::new(
                DataFormat::GmBinary,
                DetectionConfidence::Extension,
                "GM binary-file extension",
            );
        }
        "gac" => {
            return FormatDetection::new(
                DataFormat::GacBinary,
                DetectionConfidence::Extension,
                "GAC container extension",
            );
        }
        "cbf" => {
            return FormatDetection::new(
                DataFormat::CheryCbf,
                DetectionConfidence::Extension,
                "CBF container extension",
            );
        }
        _ => {}
    }
    if is_binary(content) {
        return FormatDetection::new(
            DataFormat::Binary,
            DetectionConfidence::Certain,
            "non-text bytes in input",
        );
    }

    match extension.as_str() {
        "hex" | "h86" | "ihex" | "ihx" => FormatDetection::new(
            DataFormat::IntelHex,
            DetectionConfidence::Extension,
            "Intel HEX filename extension",
        ),
        "s19" | "s28" | "s37" | "s" | "s1" | "s2" | "s3" | "sx" | "srec" => FormatDetection::new(
            DataFormat::MotorolaS,
            DetectionConfidence::Extension,
            "Motorola S-record filename extension",
        ),
        "hascii" | "hexascii" => FormatDetection::new(
            DataFormat::HexAscii,
            DetectionConfidence::Extension,
            "hexadecimal ASCII filename extension",
        ),
        "vbf" => FormatDetection::new(
            DataFormat::FordVbf,
            DetectionConfidence::Extension,
            "VBF filename extension",
        ),
        "titxt" | "ti-txt" | "ti_txt" => FormatDetection::new(
            DataFormat::TiTxt,
            DetectionConfidence::Extension,
            "TI-TXT filename extension",
        ),
        "uf2" => FormatDetection::new(
            DataFormat::Uf2,
            DetectionConfidence::Extension,
            "UF2 filename extension",
        ),
        _ => FormatDetection::new(
            DataFormat::Binary,
            DetectionConfidence::Fallback,
            "no structured format signature",
        ),
    }
}

impl DataFile {
    /// Detects and parses in-memory content. The detection result is returned
    /// so callers can report the selected format and confidence.
    pub fn parse_auto(
        path_hint: Option<&Path>,
        content: &[u8],
        segments: MemorySegmentList,
        epk_address: u32,
        epk: Option<&str>,
    ) -> Result<(Self, FormatDetection)> {
        let detection = detect_format(path_hint, content);
        let file_type = detection
            .format
            .data_file_type()
            .ok_or_else(|| Error::UnsupportedFormat(detection.format.name().to_string()))?;
        let file = Self::parse(file_type, content, segments, epk_address, epk)?;
        Ok((file, detection))
    }

    /// Reads a file once, detects its content, and dispatches to a parser.
    pub fn open_auto(
        path: &Path,
        segments: MemorySegmentList,
        epk_address: u32,
        epk: Option<&str>,
    ) -> Result<(Self, FormatDetection)> {
        let content = std::fs::read(path)?;
        let (mut file, detection) =
            Self::parse_auto(Some(path), &content, segments, epk_address, epk)?;
        file.base_mut().source_filename = Some(path.to_string_lossy().into_owned());
        Ok((file, detection))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_signatures_override_generic_extensions() {
        let detection = detect_format(Some(Path::new("wrong.bin")), b":00000001FF\n");
        assert_eq!(detection.format, DataFormat::IntelHex);
        assert_eq!(detection.confidence, DetectionConfidence::Likely);

        let detection = detect_format(Some(Path::new("wrong.hex")), b"\0\x01\x02");
        assert_eq!(detection.format, DataFormat::Binary);
        assert_eq!(detection.confidence, DetectionConfidence::Certain);

        let detection = detect_format(Some(Path::new("wrong.prm")), b":00000001FF\n");
        assert_eq!(detection.format, DataFormat::IntelHex);
    }

    #[test]
    fn detects_mixed_record_stream() {
        let detection = detect_format(None, b":021000000102EB\nS10520000304D3\n:00000001FF\n");
        assert_eq!(detection.format, DataFormat::MixedHexSRecord);
        assert_eq!(detection.confidence, DetectionConfidence::Certain);
    }

    #[test]
    fn recognizes_and_dispatches_vbf_containers() {
        let detection = detect_format(None, b"vbf_version = 2.2;\nheader {\n}.\n");
        assert_eq!(detection.format, DataFormat::FordVbf);
        assert!(detection.format.is_supported());
        let (file, _) = DataFile::parse_auto(
            None,
            b"vbf_version = 2.2;\nheader {\n}.\n",
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap();
        assert!(matches!(file, DataFile::Vbf(_)));
    }

    #[test]
    fn automatic_dispatch_supports_mixed_and_hex_ascii() {
        let (mixed, detection) = DataFile::parse_auto(
            None,
            b":021000000102EB\nS10520000304D3\n:00000001FF\n",
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap();
        assert!(matches!(mixed, DataFile::Mixed(_)));
        assert_eq!(detection.format, DataFormat::MixedHexSRecord);

        let (ascii, detection) = DataFile::parse_auto(
            Some(Path::new("bytes.hexascii")),
            b"01 02 03",
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap();
        assert!(matches!(ascii, DataFile::HexAscii(_)));
        assert_eq!(detection.format, DataFormat::HexAscii);
    }

    #[test]
    fn unknown_text_follows_hexview_binary_fallback() {
        let detection = detect_format(Some(Path::new("notes.txt")), b"ordinary text");
        assert_eq!(detection.format, DataFormat::Binary);
        assert_eq!(detection.confidence, DetectionConfidence::Fallback);
    }

    #[test]
    fn detects_and_dispatches_ti_txt_content() {
        let content = b"@F000\n01 02 03 04\nq\n";
        let detection = detect_format(Some(Path::new("firmware.bin")), content);
        assert_eq!(detection.format, DataFormat::TiTxt);
        assert_eq!(detection.confidence, DetectionConfidence::Certain);
        let (file, _) =
            DataFile::parse_auto(None, content, MemorySegmentList::new(), 0, None).unwrap();
        assert!(matches!(file, DataFile::TiTxt(_)));
    }

    #[test]
    fn detects_and_dispatches_uf2_magic() {
        use crate::uf2::{DataFileUf2, Uf2WriteOptions};

        let mut source = DataFileUf2::new(
            None,
            MemorySegmentList {
                segments: vec![crate::MemorySegment::from_data(
                    0x1000,
                    vec![1, 2, 3],
                    autors_a2l::model::enums::MemoryPrgType::DATA,
                    true,
                )],
            },
        );
        let content = source.write_uf2(Uf2WriteOptions::default()).unwrap();
        let detection = detect_format(Some(Path::new("firmware.bin")), &content);
        assert_eq!(detection.format, DataFormat::Uf2);
        assert_eq!(detection.confidence, DetectionConfidence::Certain);
        let (file, _) =
            DataFile::parse_auto(None, &content, MemorySegmentList::new(), 0, None).unwrap();
        assert!(matches!(file, DataFile::Uf2(_)));
    }
}
