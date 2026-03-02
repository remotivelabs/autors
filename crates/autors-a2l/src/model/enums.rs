#![allow(non_camel_case_types)]
#[allow(clippy::wrong_self_convention)]
pub trait A2lKeyword: Sized {
    fn as_keyword(self) -> Option<&'static str>;
    fn from_keyword(kw: &str) -> Option<Self>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangedType {
    None,
    Added,
    Changed,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddrType {
    DIRECT,
    PBYTE,
    PWORD,
    PLONG,
    PLONGLONG,
}

impl A2lKeyword for AddrType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            AddrType::DIRECT => Some("DIRECT"),
            AddrType::PBYTE => Some("PBYTE"),
            AddrType::PWORD => Some("PWORD"),
            AddrType::PLONG => Some("PLONG"),
            AddrType::PLONGLONG => Some("PLONGLONG"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "DIRECT" => Some(AddrType::DIRECT),
            "PBYTE" => Some(AddrType::PBYTE),
            "PWORD" => Some(AddrType::PWORD),
            "PLONG" => Some(AddrType::PLONG),
            "PLONGLONG" => Some(AddrType::PLONGLONG),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AxisType {
    STD_AXIS,
    FIX_AXIS,
    COM_AXIS,
    RES_AXIS,
    CURVE_AXIS,
}

impl A2lKeyword for AxisType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            AxisType::STD_AXIS => Some("STD_AXIS"),
            AxisType::FIX_AXIS => Some("FIX_AXIS"),
            AxisType::COM_AXIS => Some("COM_AXIS"),
            AxisType::RES_AXIS => Some("RES_AXIS"),
            AxisType::CURVE_AXIS => Some("CURVE_AXIS"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "STD_AXIS" => Some(AxisType::STD_AXIS),
            "FIX_AXIS" => Some(AxisType::FIX_AXIS),
            "COM_AXIS" => Some(AxisType::COM_AXIS),
            "RES_AXIS" => Some(AxisType::RES_AXIS),
            "CURVE_AXIS" => Some(AxisType::CURVE_AXIS),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AxisValueType {
    XAxis,
    YAxis,
    ZAxis,
    _4Axis,
    _5Axis,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitOperationType {
    #[default]
    NotSet,
    RIGHT_SHIFT,
    LEFT_SHIFT,
}

impl A2lKeyword for BitOperationType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            BitOperationType::RIGHT_SHIFT => Some("RIGHT_SHIFT"),
            BitOperationType::LEFT_SHIFT => Some("LEFT_SHIFT"),
            BitOperationType::NotSet => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "RIGHT_SHIFT" => Some(BitOperationType::RIGHT_SHIFT),
            "LEFT_SHIFT" => Some(BitOperationType::LEFT_SHIFT),
            _ => None,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalibrationAccess {
    #[default]
    NotSet,
    CALIBRATION,
    NO_CALIBRATION,
    NOT_IN_MCD_SYSTEM,
    OFFLINE_CALIBRATION,
}

impl A2lKeyword for CalibrationAccess {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            CalibrationAccess::CALIBRATION => Some("CALIBRATION"),
            CalibrationAccess::NO_CALIBRATION => Some("NO_CALIBRATION"),
            CalibrationAccess::NOT_IN_MCD_SYSTEM => Some("NOT_IN_MCD_SYSTEM"),
            CalibrationAccess::OFFLINE_CALIBRATION => Some("OFFLINE_CALIBRATION"),
            CalibrationAccess::NotSet => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "CALIBRATION" => Some(CalibrationAccess::CALIBRATION),
            "NO_CALIBRATION" => Some(CalibrationAccess::NO_CALIBRATION),
            "NOT_IN_MCD_SYSTEM" => Some(CalibrationAccess::NOT_IN_MCD_SYSTEM),
            "OFFLINE_CALIBRATION" => Some(CalibrationAccess::OFFLINE_CALIBRATION),
            _ => None,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChecksumType {
    #[default]
    NotSet,
    ADD_11,
    ADD_12,
    ADD_14,
    CRC_8,
    CRC_16,
    CRC_32,
    CRC_2_16,
    ADD_22,
    ADD_24,
    ADD_44,
    USER_DEFINED,
    CRC_16_CITT,
}

impl A2lKeyword for ChecksumType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            ChecksumType::ADD_11 => Some("ADD_11"),
            ChecksumType::ADD_12 => Some("ADD_12"),
            ChecksumType::ADD_14 => Some("ADD_14"),
            ChecksumType::CRC_8 => Some("CRC_8"),
            ChecksumType::CRC_16 => Some("CRC_16"),
            ChecksumType::CRC_32 => Some("CRC_32"),
            ChecksumType::CRC_2_16 => Some("CRC_2_16"),
            ChecksumType::ADD_22 => Some("ADD_22"),
            ChecksumType::ADD_24 => Some("ADD_24"),
            ChecksumType::ADD_44 => Some("ADD_44"),
            ChecksumType::USER_DEFINED => Some("USER_DEFINED"),
            ChecksumType::CRC_16_CITT => Some("CRC_16_CITT"),
            ChecksumType::NotSet => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "ADD_11" => Some(ChecksumType::ADD_11),
            "ADD_12" => Some(ChecksumType::ADD_12),
            "ADD_14" => Some(ChecksumType::ADD_14),
            "CRC_8" => Some(ChecksumType::CRC_8),
            "CRC_16" => Some(ChecksumType::CRC_16),
            "CRC_32" => Some(ChecksumType::CRC_32),
            "CRC_2_16" => Some(ChecksumType::CRC_2_16),
            "ADD_22" => Some(ChecksumType::ADD_22),
            "ADD_24" => Some(ChecksumType::ADD_24),
            "ADD_44" => Some(ChecksumType::ADD_44),
            "USER_DEFINED" => Some(ChecksumType::USER_DEFINED),
            "CRC_16_CITT" => Some(ChecksumType::CRC_16_CITT),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversionType {
    IDENTICAL,
    FORM,
    LINEAR,
    RAT_FUNC,
    TAB_INTP,
    TAB_NOINTP,
    TAB_VERB,
}

impl A2lKeyword for ConversionType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            ConversionType::IDENTICAL => Some("IDENTICAL"),
            ConversionType::FORM => Some("FORM"),
            ConversionType::LINEAR => Some("LINEAR"),
            ConversionType::RAT_FUNC => Some("RAT_FUNC"),
            ConversionType::TAB_INTP => Some("TAB_INTP"),
            ConversionType::TAB_NOINTP => Some("TAB_NOINTP"),
            ConversionType::TAB_VERB => Some("TAB_VERB"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "IDENTICAL" => Some(ConversionType::IDENTICAL),
            "FORM" => Some(ConversionType::FORM),
            "LINEAR" => Some(ConversionType::LINEAR),
            "RAT_FUNC" => Some(ConversionType::RAT_FUNC),
            "TAB_INTP" => Some(ConversionType::TAB_INTP),
            "TAB_NOINTP" => Some(ConversionType::TAB_NOINTP),
            "TAB_VERB" => Some(ConversionType::TAB_VERB),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DaqEventType {
    None,
    XCP,
    CCP,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataSize {
    BYTE,
    WORD,
    LONG,
}

impl A2lKeyword for DataSize {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            DataSize::BYTE => Some("BYTE"),
            DataSize::WORD => Some("WORD"),
            DataSize::LONG => Some("LONG"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "BYTE" => Some(DataSize::BYTE),
            "WORD" => Some(DataSize::WORD),
            "LONG" => Some(DataSize::LONG),
            _ => None,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepositType {
    #[default]
    NotSet,
    ABSOLUTE,
    DIFFERENCE,
}

impl A2lKeyword for DepositType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            DepositType::ABSOLUTE => Some("ABSOLUTE"),
            DepositType::DIFFERENCE => Some("DIFFERENCE"),
            DepositType::NotSet => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "ABSOLUTE" => Some(DepositType::ABSOLUTE),
            "DIFFERENCE" => Some(DepositType::DIFFERENCE),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EcuPage {
    Flash = 0,
    RAM = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncodingType {
    ASCII,
    UTF8,
    UTF16,
    UTF32,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexMode {
    #[default]
    NotSet,
    ROW_DIR,
    COLUMN_DIR,
    ALTERNATE_WITH_X,
    ALTERNATE_WITH_Y,
    ALTERNATE_CURVES,
}

impl A2lKeyword for IndexMode {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            IndexMode::ROW_DIR => Some("ROW_DIR"),
            IndexMode::COLUMN_DIR => Some("COLUMN_DIR"),
            IndexMode::ALTERNATE_WITH_X => Some("ALTERNATE_WITH_X"),
            IndexMode::ALTERNATE_WITH_Y => Some("ALTERNATE_WITH_Y"),
            IndexMode::ALTERNATE_CURVES => Some("ALTERNATE_CURVES"),
            IndexMode::NotSet => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "ROW_DIR" => Some(IndexMode::ROW_DIR),
            "COLUMN_DIR" => Some(IndexMode::COLUMN_DIR),
            "ALTERNATE_WITH_X" => Some(IndexMode::ALTERNATE_WITH_X),
            "ALTERNATE_WITH_Y" => Some(IndexMode::ALTERNATE_WITH_Y),
            "ALTERNATE_CURVES" => Some(IndexMode::ALTERNATE_CURVES),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexOrder {
    INDEX_INCR,
    INDEX_DECR,
}

impl A2lKeyword for IndexOrder {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            IndexOrder::INDEX_INCR => Some("INDEX_INCR"),
            IndexOrder::INDEX_DECR => Some("INDEX_DECR"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "INDEX_INCR" => Some(IndexOrder::INDEX_INCR),
            "INDEX_DECR" => Some(IndexOrder::INDEX_DECR),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryAttribute {
    INTERN,
    EXTERN,
}

impl A2lKeyword for MemoryAttribute {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            MemoryAttribute::INTERN => Some("INTERN"),
            MemoryAttribute::EXTERN => Some("EXTERN"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "INTERN" => Some(MemoryAttribute::INTERN),
            "EXTERN" => Some(MemoryAttribute::EXTERN),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryType {
    RAM,
    EEPROM,
    EPROM,
    ROM,
    REGISTER,
    FLASH,
    NOT_IN_ECU,
}

impl A2lKeyword for MemoryType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            MemoryType::RAM => Some("RAM"),
            MemoryType::EEPROM => Some("EEPROM"),
            MemoryType::EPROM => Some("EPROM"),
            MemoryType::ROM => Some("ROM"),
            MemoryType::REGISTER => Some("REGISTER"),
            MemoryType::FLASH => Some("FLASH"),
            MemoryType::NOT_IN_ECU => Some("NOT_IN_ECU"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "RAM" => Some(MemoryType::RAM),
            "EEPROM" => Some(MemoryType::EEPROM),
            "EPROM" => Some(MemoryType::EPROM),
            "ROM" => Some(MemoryType::ROM),
            "REGISTER" => Some(MemoryType::REGISTER),
            "FLASH" => Some(MemoryType::FLASH),
            "NOT_IN_ECU" => Some(MemoryType::NOT_IN_ECU),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryPrgType {
    UNKNOWN,
    CODE,
    DATA,
    OFFLINE_DATA,
    VARIABLES,
    SERAM,
    RESERVED,
    CALIBRATION_VARIABLES,
    EXCLUDE_FROM_FLASH,
}

impl A2lKeyword for MemoryPrgType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            MemoryPrgType::UNKNOWN => Some("UNKNOWN"),
            MemoryPrgType::CODE => Some("CODE"),
            MemoryPrgType::DATA => Some("DATA"),
            MemoryPrgType::OFFLINE_DATA => Some("OFFLINE_DATA"),
            MemoryPrgType::VARIABLES => Some("VARIABLES"),
            MemoryPrgType::SERAM => Some("SERAM"),
            MemoryPrgType::RESERVED => Some("RESERVED"),
            MemoryPrgType::CALIBRATION_VARIABLES => Some("CALIBRATION_VARIABLES"),
            MemoryPrgType::EXCLUDE_FROM_FLASH => Some("EXCLUDE_FROM_FLASH"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "UNKNOWN" => Some(MemoryPrgType::UNKNOWN),
            "CODE" => Some(MemoryPrgType::CODE),
            "DATA" => Some(MemoryPrgType::DATA),
            "OFFLINE_DATA" => Some(MemoryPrgType::OFFLINE_DATA),
            "VARIABLES" => Some(MemoryPrgType::VARIABLES),
            "SERAM" => Some(MemoryPrgType::SERAM),
            "RESERVED" => Some(MemoryPrgType::RESERVED),
            "CALIBRATION_VARIABLES" => Some(MemoryPrgType::CALIBRATION_VARIABLES),
            "EXCLUDE_FROM_FLASH" => Some(MemoryPrgType::EXCLUDE_FROM_FLASH),
            _ => None,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MonotonyType {
    #[default]
    NotSet,
    NOT_MON,
    MON_DECREASE,
    MON_INCREASE,
    STRICT_DECREASE,
    STRICT_INCREASE,
    MONOTONOUS,
    STRICT_MON,
}

impl A2lKeyword for MonotonyType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            MonotonyType::NOT_MON => Some("NOT_MON"),
            MonotonyType::MON_DECREASE => Some("MON_DECREASE"),
            MonotonyType::MON_INCREASE => Some("MON_INCREASE"),
            MonotonyType::STRICT_DECREASE => Some("STRICT_DECREASE"),
            MonotonyType::STRICT_INCREASE => Some("STRICT_INCREASE"),
            MonotonyType::MONOTONOUS => Some("MONOTONOUS"),
            MonotonyType::STRICT_MON => Some("STRICT_MON"),
            MonotonyType::NotSet => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "NOT_MON" => Some(MonotonyType::NOT_MON),
            "MON_DECREASE" => Some(MonotonyType::MON_DECREASE),
            "MON_INCREASE" => Some(MonotonyType::MON_INCREASE),
            "STRICT_DECREASE" => Some(MonotonyType::STRICT_DECREASE),
            "STRICT_INCREASE" => Some(MonotonyType::STRICT_INCREASE),
            "MONOTONOUS" => Some(MonotonyType::MONOTONOUS),
            "STRICT_MON" => Some(MonotonyType::STRICT_MON),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrgType {
    PRG_CODE,
    PRG_DATA,
    PRG_RESERVED,
}

impl A2lKeyword for PrgType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            PrgType::PRG_CODE => Some("PRG_CODE"),
            PrgType::PRG_DATA => Some("PRG_DATA"),
            PrgType::PRG_RESERVED => Some("PRG_RESERVED"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "PRG_CODE" => Some(PrgType::PRG_CODE),
            "PRG_DATA" => Some(PrgType::PRG_DATA),
            "PRG_RESERVED" => Some(PrgType::PRG_RESERVED),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalingUnits {
    Time_1uSec = 0,
    Time_10uSec = 1,
    Time_100uSec = 2,
    Time_1mSec = 3,
    Time_10mSec = 4,
    Time_100mSec = 5,
    Time_1Sec = 6,
    Time_10Sec = 7,
    Time_1Min = 8,
    Time_1Hour = 9,
    Time_1Day = 10,
    AngularDegrees = 100,
    Revolutions = 101,
    Cycle = 102,
    CylinderSegmentCombustion = 103,
    FrameAvailableEvent = 998,
    AlwaysOnNewValue = 999,
    NonDeternministic = 1000,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerType {
    ON_CHANGE,
    ON_USER_REQUEST,
}

impl A2lKeyword for TriggerType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            TriggerType::ON_CHANGE => Some("ON_CHANGE"),
            TriggerType::ON_USER_REQUEST => Some("ON_USER_REQUEST"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "ON_CHANGE" => Some(TriggerType::ON_CHANGE),
            "ON_USER_REQUEST" => Some(TriggerType::ON_USER_REQUEST),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitType {
    DERIVED,
    EXTENDED_SI,
}

impl A2lKeyword for UnitType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            UnitType::DERIVED => Some("DERIVED"),
            UnitType::EXTENDED_SI => Some("EXTENDED_SI"),
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "DERIVED" => Some(UnitType::DERIVED),
            "EXTENDED_SI" => Some(UnitType::EXTENDED_SI),
            _ => None,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VarNamingType {
    #[default]
    NotSet,
    NUMERIC,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriterIndent {
    Space,
    Tab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriterSortMode {
    ByName,
    ByTypeAndName,
    ByAddressAndName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanapeCharType {
    Kw,
    Kwb,
    Kl,
    Gkl,
    Kf,
    Gkf,
    Fkl,
    Fkf,
    GSst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanapeCompuType {
    Poly,
    TabInt,
    TabNint,
    Verbal,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanapeDataBaseType {
    #[default]
    Unsupported,
    B,
    W,
    L,
    Ll,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanapeDataType {
    #[default]
    Unsupported,
    Ub,
    Sb,
    Uw,
    Sw,
    Ul,
    Sl,
    Fl,
    Ull,
    Sll,
    Fl64,
    Fl16,
    Ubit,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanapeObjectType {
    #[default]
    Unsupported,
    OSp,
    Umr,
    Kgs,
    Abl,
    Ram,
    Fkt,
    LokRam,
    RefRam,
    RefKgs,
    AdL,
    Bez,
    Datum,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataType {
    #[default]
    Unsupported,
    UByte,
    SByte,
    UWord,
    SWord,
    ULong,
    SLong,
    AUInt64,
    AInt64,
    Float16Ieee,
    Float32Ieee,
    Float64Ieee,
}

impl A2lKeyword for DataType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            DataType::UByte => Some("UBYTE"),
            DataType::SByte => Some("SBYTE"),
            DataType::UWord => Some("UWORD"),
            DataType::SWord => Some("SWORD"),
            DataType::ULong => Some("ULONG"),
            DataType::SLong => Some("SLONG"),
            DataType::AUInt64 => Some("A_UINT64"),
            DataType::AInt64 => Some("A_INT64"),
            DataType::Float16Ieee => Some("FLOAT16_IEEE"),
            DataType::Float32Ieee => Some("FLOAT32_IEEE"),
            DataType::Float64Ieee => Some("FLOAT64_IEEE"),
            DataType::Unsupported => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "UBYTE" => Some(DataType::UByte),
            "SBYTE" => Some(DataType::SByte),
            "UWORD" => Some(DataType::UWord),
            "SWORD" => Some(DataType::SWord),
            "ULONG" => Some(DataType::ULong),
            "SLONG" => Some(DataType::SLong),
            "A_UINT64" => Some(DataType::AUInt64),
            "A_INT64" => Some(DataType::AInt64),
            "FLOAT16_IEEE" => Some(DataType::Float16Ieee),
            "FLOAT32_IEEE" => Some(DataType::Float32Ieee),
            "FLOAT64_IEEE" => Some(DataType::Float64Ieee),
            _ => None,
        }
    }
}

#[allow(non_camel_case_types)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CharacteristicType {
    #[default]
    NotSet,
    VALUE,
    ASCII,
    VAL_BLK,
    CURVE,
    MAP,
    CUBOID,
    CUBE_4,
    CUBE_5,
}

impl A2lKeyword for CharacteristicType {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            CharacteristicType::VALUE => Some("VALUE"),
            CharacteristicType::ASCII => Some("ASCII"),
            CharacteristicType::VAL_BLK => Some("VAL_BLK"),
            CharacteristicType::CURVE => Some("CURVE"),
            CharacteristicType::MAP => Some("MAP"),
            CharacteristicType::CUBOID => Some("CUBOID"),
            CharacteristicType::CUBE_4 => Some("CUBE_4"),
            CharacteristicType::CUBE_5 => Some("CUBE_5"),
            CharacteristicType::NotSet => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "VALUE" => Some(CharacteristicType::VALUE),
            "ASCII" => Some(CharacteristicType::ASCII),
            "VAL_BLK" => Some(CharacteristicType::VAL_BLK),
            "CURVE" => Some(CharacteristicType::CURVE),
            "MAP" => Some(CharacteristicType::MAP),
            "CUBOID" => Some(CharacteristicType::CUBOID),
            "CUBE_4" => Some(CharacteristicType::CUBE_4),
            "CUBE_5" => Some(CharacteristicType::CUBE_5),
            _ => None,
        }
    }
}
