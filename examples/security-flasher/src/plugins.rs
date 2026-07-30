use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};

use autors_security_flasher_sdk::{
    file_loader_struct_size, flow_struct_size, FileLoaderEntryV1, FileLoaderPluginV1, FlowEntryV1,
    FlowPluginV1, SegmentSinkV1, ABI_VERSION_V1, FILE_LOADER_ENTRY_SYMBOL, FLOW_ENTRY_SYMBOL,
    STATUS_ERROR, STATUS_OK,
};
use libloading::Library;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct FlashSegment {
    pub address: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct FlashImage {
    pub segments: Vec<FlashSegment>,
}

pub struct LoadedFlow {
    _library: Library,
    api: FlowPluginV1,
    pub name: String,
    pub path: PathBuf,
}

impl LoadedFlow {
    pub fn load(path: &Path) -> Result<Self> {
        let resolved = resolve_library_path(path)?;
        // SAFETY: the library stays owned by `Self` for at least as long as
        // every copied function pointer in `api` can be called.
        let library = unsafe { Library::new(&resolved) }
            .map_err(|error| Error::Plugin(format!("{}: {error}", resolved.display())))?;
        // SAFETY: the symbol has a versioned, documented C ABI. Its returned
        // table is copied while the library is still loaded.
        let api = unsafe {
            let entry = library
                .get::<FlowEntryV1>(FLOW_ENTRY_SYMBOL)
                .map_err(|error| Error::Plugin(format!("{}: {error}", resolved.display())))?;
            let pointer = entry();
            if pointer.is_null() {
                return Err(Error::Plugin(format!(
                    "{} returned a null flow API",
                    resolved.display()
                )));
            }
            *pointer
        };
        validate_flow_api(&api, &resolved)?;
        // SAFETY: validation above guarantees a callable function pointer;
        // plugin names are required to point to a static NUL-terminated string.
        let name = unsafe { copy_c_string((api.plugin_name)(), "flow plugin name")? };
        Ok(Self {
            _library: library,
            api,
            name,
            path: resolved,
        })
    }

    pub fn execute(&self, context: &autors_security_flasher_sdk::FlowContextV1) -> i32 {
        // SAFETY: the context table and all pointers it contains remain valid
        // for the duration of this synchronous call.
        unsafe { (self.api.execute)(context) }
    }
}

pub struct LoadedFileLoader {
    _library: Library,
    api: FileLoaderPluginV1,
    pub name: String,
    pub path: PathBuf,
}

impl LoadedFileLoader {
    pub fn load(path: &Path) -> Result<Self> {
        let resolved = resolve_library_path(path)?;
        // SAFETY: the library is retained by `Self` while its API is in use.
        let library = unsafe { Library::new(&resolved) }
            .map_err(|error| Error::Plugin(format!("{}: {error}", resolved.display())))?;
        // SAFETY: the entry point uses the versioned C ABI from the SDK.
        let api = unsafe {
            let entry = library
                .get::<FileLoaderEntryV1>(FILE_LOADER_ENTRY_SYMBOL)
                .map_err(|error| Error::Plugin(format!("{}: {error}", resolved.display())))?;
            let pointer = entry();
            if pointer.is_null() {
                return Err(Error::Plugin(format!(
                    "{} returned a null file-loader API",
                    resolved.display()
                )));
            }
            *pointer
        };
        validate_loader_api(&api, &resolved)?;
        // SAFETY: plugin names are static NUL-terminated strings by contract.
        let name = unsafe { copy_c_string((api.plugin_name)(), "file-loader plugin name")? };
        Ok(Self {
            _library: library,
            api,
            name,
            path: resolved,
        })
    }

    pub fn load_image(&self, path: &Path, base_address: u32, component: u32) -> Result<FlashImage> {
        let path_text = path.to_string_lossy();
        let path_c = CString::new(path_text.as_bytes())
            .map_err(|_| Error::Plugin("image path contains a NUL byte".to_string()))?;
        // SAFETY: `path_c` remains alive for this call and the plugin promises
        // not to retain its pointer.
        if unsafe { (self.api.can_load)(path_c.as_ptr(), component) } == 0 {
            return Err(Error::Plugin(format!(
                "{} does not accept {} for component {component}",
                self.name,
                path.display()
            )));
        }

        let mut collector = SegmentCollector::default();
        let sink = SegmentSinkV1 {
            abi_version: ABI_VERSION_V1,
            user_data: (&mut collector as *mut SegmentCollector).cast::<c_void>(),
            push_segment,
            set_error: set_collector_error,
        };
        // SAFETY: the path and sink remain valid for this synchronous call;
        // `push_segment` copies every byte before returning.
        let status = unsafe { (self.api.load)(path_c.as_ptr(), base_address, component, &sink) };
        if status != STATUS_OK {
            return Err(Error::Plugin(collector.error.unwrap_or_else(|| {
                format!("{} failed with status {status}", self.name)
            })));
        }
        if let Some(error) = collector.error {
            return Err(Error::Plugin(error));
        }
        collector.segments.sort_by_key(|segment| segment.address);
        validate_segments(&collector.segments, path)?;
        Ok(FlashImage {
            segments: collector.segments,
        })
    }
}

#[derive(Default)]
struct SegmentCollector {
    segments: Vec<FlashSegment>,
    error: Option<String>,
}

unsafe extern "C" fn push_segment(
    user_data: *mut c_void,
    address: u32,
    data: *const u8,
    data_len: usize,
) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if user_data.is_null() || data.is_null() || data_len == 0 {
            return STATUS_ERROR;
        }
        // SAFETY: the loader owns `data` for at least this callback and the
        // host supplied `user_data` as a live `SegmentCollector` pointer.
        let (collector, bytes) = unsafe {
            (
                &mut *user_data.cast::<SegmentCollector>(),
                std::slice::from_raw_parts(data, data_len),
            )
        };
        if u64::from(address) + data_len as u64 > u64::from(u32::MAX) + 1 {
            collector.error = Some(format!(
                "segment at 0x{address:08X} exceeds the 32-bit address space"
            ));
            return STATUS_ERROR;
        }
        collector.segments.push(FlashSegment {
            address,
            data: bytes.to_vec(),
        });
        STATUS_OK
    }));
    result.unwrap_or(STATUS_ERROR)
}

unsafe extern "C" fn set_collector_error(user_data: *mut c_void, message: *const std::ffi::c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if user_data.is_null() || message.is_null() {
            return;
        }
        // SAFETY: both pointers are valid for this callback by the ABI contract.
        let collector = unsafe { &mut *user_data.cast::<SegmentCollector>() };
        // SAFETY: plugin error strings must be NUL-terminated and stay alive
        // for the duration of this callback.
        collector.error = Some(
            unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned(),
        );
    }));
}

fn validate_flow_api(api: &FlowPluginV1, path: &Path) -> Result<()> {
    if api.abi_version != ABI_VERSION_V1 || api.struct_size < flow_struct_size() {
        return Err(Error::Plugin(format!(
            "{} exposes an incompatible flow ABI (version {}, size {})",
            path.display(),
            api.abi_version,
            api.struct_size
        )));
    }
    Ok(())
}

fn validate_loader_api(api: &FileLoaderPluginV1, path: &Path) -> Result<()> {
    if api.abi_version != ABI_VERSION_V1 || api.struct_size < file_loader_struct_size() {
        return Err(Error::Plugin(format!(
            "{} exposes an incompatible file-loader ABI (version {}, size {})",
            path.display(),
            api.abi_version,
            api.struct_size
        )));
    }
    Ok(())
}

fn validate_segments(segments: &[FlashSegment], path: &Path) -> Result<()> {
    if segments.is_empty() {
        return Err(Error::Plugin(format!(
            "{} contains no loadable segments",
            path.display()
        )));
    }
    for pair in segments.windows(2) {
        let previous_end = u64::from(pair[0].address) + pair[0].data.len() as u64;
        if previous_end > u64::from(pair[1].address) {
            return Err(Error::Plugin(format!(
                "{} contains overlapping segments at 0x{:08X}",
                path.display(),
                pair[1].address
            )));
        }
    }
    Ok(())
}

pub fn resolve_library_path(path: &Path) -> Result<PathBuf> {
    let mut candidates = vec![path.to_path_buf()];
    if let Ok(executable) = std::env::current_exe() {
        if let (Some(directory), Some(file_name)) = (executable.parent(), path.file_name()) {
            candidates.push(directory.join(file_name));
            if directory.file_name().is_some_and(|name| name == "deps") {
                if let Some(target_profile_dir) = directory.parent() {
                    candidates.push(target_profile_dir.join(file_name));
                }
            }
            #[cfg(not(windows))]
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                let extension = if cfg!(target_os = "macos") {
                    "dylib"
                } else {
                    "so"
                };
                candidates.push(directory.join(format!("lib{stem}.{extension}")));
            }
        }
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| Error::Plugin(format!("dynamic library not found: {}", path.display())))
}

unsafe fn copy_c_string(pointer: *const std::ffi::c_char, label: &str) -> Result<String> {
    if pointer.is_null() {
        return Err(Error::Plugin(format!("{label} is null")));
    }
    // SAFETY: the caller established that `pointer` is a plugin-owned,
    // NUL-terminated string valid for this call.
    Ok(unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned())
}
