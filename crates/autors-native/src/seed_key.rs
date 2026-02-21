#[cfg(windows)]
use crate::dll::DllWrapper;
#[cfg(windows)]
use crate::error::Result;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SkType(pub u8);

impl SkType {
    pub const UNKNOWN: Self = Self(0);
    /// CCP Seed&Key.
    pub const CCP: Self = Self(1);
    /// XCP Seed&Key.
    pub const XCP: Self = Self(2);
    /// UDS Seed&Key(GenerateKeyEx).
    pub const UDS: Self = Self(4);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for SkType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for SkType {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ResultSk {
    FncAck = 0,
    ErrPrivilegeNotAvailable = 1,
    ErrInvalidSeedLength = 2,
    ErrUnsufficientKeyLength = 3,
    ErrGlobal = 4,
}

impl ResultSk {
    pub fn from_raw(v: i32) -> Self {
        match v {
            0 => Self::FncAck,
            1 => Self::ErrPrivilegeNotAvailable,
            2 => Self::ErrInvalidSeedLength,
            3 => Self::ErrUnsufficientKeyLength,
            _ => Self::ErrGlobal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum VKeyGenResultEx {
    Ok = 0,
    BufferToSmall = 1,
    SecurityLevelInvalid = 2,
    VariantInvalid = 3,
    UnspecifiedError = 4,
}

impl VKeyGenResultEx {
    pub fn from_raw(v: i32) -> Self {
        match v {
            0 => Self::Ok,
            1 => Self::BufferToSmall,
            2 => Self::SecurityLevelInvalid,
            3 => Self::VariantInvalid,
            _ => Self::UnspecifiedError,
        }
    }
}

/// - XCP:`XCP_GetAvailablePrivileges` + `XCP_ComputeKeyFromSeed`
/// - UDS:`GenerateKeyEx`
#[cfg(windows)]
pub struct NativeSkDll {
    dll: DllWrapper,
    sk_type: SkType,
    xcp_get_available_privileges: Option<XcpGetAvailablePrivilegesFn>,
    xcp_compute_key_from_seed: Option<XcpComputeKeyFromSeedFn>,
    ccp_key_gen_string: Option<CcpKeyGenStringFn>,
    ccp_compute_key_from_seed: Option<CcpComputeKeyFromSeedFn>,
    uds_generate_key_ex: Option<GenerateKeyExFn>,
}

#[cfg(windows)]
type XcpGetAvailablePrivilegesFn = unsafe extern "C" fn(privileges: *mut u8) -> i32;
#[cfg(windows)]
type XcpComputeKeyFromSeedFn = unsafe extern "C" fn(
    privilege: u8,
    seed_len: u8,
    seed: *const u8,
    key_len: *mut u8,
    key: *mut u8,
) -> i32;
#[cfg(windows)]
type CcpComputeKeyFromSeedFn = unsafe extern "C" fn(
    seed: *const u8,
    seed_len: u16,
    key: *mut u8,
    max_key_len: u16,
    result_len: *mut u16,
) -> i32;
#[cfg(windows)]
type CcpKeyGenStringFn = unsafe extern "C" fn(seed: *const std::ffi::c_char, key: *mut u32) -> u32;
#[cfg(windows)]
type GenerateKeyExFn = unsafe extern "C" fn(
    seed: *const u8,
    seed_len: u32,
    security_level: u32,
    variant: *const u8,
    key: *mut u8,
    max_key_len: u32,
    actual_len: *mut u32,
) -> i32;

#[cfg(windows)]
const CCP_KEYGEN_STRING_NAMES: [&str; 3] = ["KeyGen", "GenerateKey", "ASAP1A_KeyGen"];

#[cfg(windows)]
impl NativeSkDll {
    const CCP_KEYGEN_SEED_ITEM_FMT: fn(&mut String, u8) = |s, b| {
        s.push_str(&format!("{b:02X} "));
    };

    pub fn new(path_to_dll: impl Into<String>) -> Result<Self> {
        let dll = DllWrapper::load(path_to_dll)?;
        let (xcp_get, xcp_compute, ccp_bin, ccp_str, uds_gen) = unsafe {
            let xcp_get =
                dll.resolve_fn::<XcpGetAvailablePrivilegesFn>("XCP_GetAvailablePrivileges");
            let xcp_compute = dll.resolve_fn::<XcpComputeKeyFromSeedFn>("XCP_ComputeKeyFromSeed");
            let ccp_bin =
                dll.resolve_fn::<CcpComputeKeyFromSeedFn>("ASAP1A_CCP_ComputeKeyFromSeed");
            let ccp_str = CCP_KEYGEN_STRING_NAMES
                .iter()
                .find_map(|n| dll.resolve_fn::<CcpKeyGenStringFn>(n));
            let uds_gen = dll.resolve_fn::<GenerateKeyExFn>("GenerateKeyEx");
            (xcp_get, xcp_compute, ccp_bin, ccp_str, uds_gen)
        };
        let mut sk_type = SkType::UNKNOWN;
        if xcp_get.is_some() && xcp_compute.is_some() {
            sk_type |= SkType::XCP;
        }
        if ccp_bin.is_some() || ccp_str.is_some() {
            sk_type |= SkType::CCP;
        }
        if uds_gen.is_some() {
            sk_type |= SkType::UDS;
        }
        Ok(Self {
            dll,
            sk_type,
            xcp_get_available_privileges: xcp_get,
            xcp_compute_key_from_seed: xcp_compute,
            ccp_key_gen_string: ccp_str,
            ccp_compute_key_from_seed: ccp_bin,
            uds_generate_key_ex: uds_gen,
        })
    }

    pub fn sk_type(&self) -> SkType {
        self.sk_type
    }

    pub fn file_path(&self) -> &str {
        self.dll.file_path()
    }

    pub fn get_available_privileges(&self, privileges: &mut u8) -> ResultSk {
        match self.xcp_get_available_privileges {
            None => ResultSk::ErrGlobal,
            Some(f) => ResultSk::from_raw(unsafe { f(privileges as *mut u8) }),
        }
    }

    pub fn compute_key_from_seed_ccp(&self, seed: &[u8]) -> Option<Vec<u8>> {
        if !self.sk_type.contains(SkType::CCP) {
            return None;
        }
        if let Some(f) = self.ccp_key_gen_string {
            let mut s = String::new();
            for &b in seed {
                (Self::CCP_KEYGEN_SEED_ITEM_FMT)(&mut s, b);
            }
            s.pop();
            let cstr = std::ffi::CString::new(s).ok()?;
            let mut key = 0u32;
            unsafe { f(cstr.as_ptr(), &mut key as *mut u32) };
            return Some(key.to_le_bytes().to_vec());
        }
        if let Some(f) = self.ccp_compute_key_from_seed {
            let mut buf = vec![0u8; seed.len()];
            let mut result_len = 0u16;
            let ok = unsafe {
                f(
                    seed.as_ptr(),
                    seed.len() as u16,
                    buf.as_mut_ptr(),
                    buf.len() as u16,
                    &mut result_len as *mut u16,
                )
            };
            if ok != 0 && result_len > 0 && usize::from(result_len) <= buf.len() {
                buf.truncate(usize::from(result_len));
                return Some(buf);
            }
            return if ok != 0 { Some(Vec::new()) } else { None };
        }
        None
    }

    pub fn compute_key_from_seed_xcp(
        &self,
        privilege: u8,
        seed: &[u8],
    ) -> (ResultSk, Option<Vec<u8>>) {
        let Some(f) = self.xcp_compute_key_from_seed else {
            return (ResultSk::ErrGlobal, None);
        };
        if seed.len() > 255 {
            return (ResultSk::ErrGlobal, None);
        }
        let mut key_len = u8::MAX;
        let mut key = vec![0u8; usize::from(u8::MAX)];
        let raw = unsafe {
            f(
                privilege,
                seed.len() as u8,
                seed.as_ptr(),
                &mut key_len as *mut u8,
                key.as_mut_ptr(),
            )
        };
        let result = ResultSk::from_raw(raw);
        if result == ResultSk::FncAck {
            key.truncate(usize::from(key_len));
            (result, Some(key))
        } else {
            (result, None)
        }
    }

    pub fn compute_key_from_seed_uds(
        &self,
        security_level: u32,
        variant: &[u8],
        seed: &[u8],
    ) -> (VKeyGenResultEx, Option<Vec<u8>>) {
        if self.sk_type.contains(SkType::UDS) {
            if let Some(f) = self.uds_generate_key_ex {
                let max_len = 255u32;
                let mut buf = vec![0u8; max_len as usize];
                let mut actual = 0u32;
                let raw = unsafe {
                    f(
                        seed.as_ptr(),
                        seed.len() as u32,
                        security_level,
                        variant.as_ptr(),
                        buf.as_mut_ptr(),
                        max_len,
                        &mut actual as *mut u32,
                    )
                };
                let result = VKeyGenResultEx::from_raw(raw);
                if result == VKeyGenResultEx::Ok && actual != 0 && actual <= max_len {
                    buf.truncate(actual as usize);
                    return (result, Some(buf));
                }
                return (result, None);
            }
        }
        match self.compute_key_from_seed_ccp(seed) {
            Some(key) => (VKeyGenResultEx::Ok, Some(key)),
            None => (VKeyGenResultEx::UnspecifiedError, None),
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_reflected_values() {
        assert_eq!(SkType::UNKNOWN.bits(), 0);
        assert_eq!(SkType::CCP.bits(), 1);
        assert_eq!(SkType::XCP.bits(), 2);
        assert_eq!(SkType::UDS.bits(), 4);
        assert!((SkType::CCP | SkType::XCP).contains(SkType::XCP));
        assert_eq!(ResultSk::FncAck as i32, 0);
        assert_eq!(ResultSk::from_raw(4), ResultSk::ErrGlobal);
        assert_eq!(ResultSk::from_raw(99), ResultSk::ErrGlobal);
        assert_eq!(VKeyGenResultEx::from_raw(1), VKeyGenResultEx::BufferToSmall);
        assert_eq!(
            VKeyGenResultEx::from_raw(-1),
            VKeyGenResultEx::UnspecifiedError
        );
    }
}
