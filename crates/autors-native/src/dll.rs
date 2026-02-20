use std::fmt;

use crate::error::Result;

pub struct DllWrapper {
    lib: libloading::Library,
    file_path: String,
}

impl DllWrapper {
    pub fn load(path_to_dll: impl Into<String>) -> Result<Self> {
        let file_path = path_to_dll.into();
        let lib = unsafe { libloading::Library::new(&file_path)? };
        Ok(Self { lib, file_path })
    }

    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    pub fn file_version(&self) -> Option<&str> {
        None
    }

    pub fn library(&self) -> &libloading::Library {
        &self.lib
    }

    /// # Safety
    pub unsafe fn resolve_fn<T: Copy>(&self, name: &str) -> Option<T> {
        unsafe { self.lib.get::<T>(name.as_bytes()) }
            .ok()
            .map(|s| *s)
    }
}

impl fmt::Debug for DllWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DllWrapper")
            .field("file_path", &self.file_path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dll_wrapper_load_failure() {
        assert!(DllWrapper::load("definitely_not_existing_12345.dll").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn dll_wrapper_load_kernel32() {
        let w = DllWrapper::load("kernel32.dll").unwrap();
        assert!(w.file_path().ends_with("kernel32.dll"));
        let p: Option<unsafe extern "C" fn() -> u32> = unsafe { w.resolve_fn("GetTickCount") };
        assert!(p.is_some());
        assert!(w.file_version().is_none());
    }
}
