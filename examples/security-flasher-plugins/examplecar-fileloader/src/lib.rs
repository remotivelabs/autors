use std::ffi::{c_char, CStr, CString};
use std::path::Path;

use autors_datafile::datafile::{DataFile, MemorySegmentList};
use autors_security_flasher_sdk::{
    file_loader_struct_size, FileLoaderPluginV1, SegmentSinkV1, ABI_VERSION_V1,
    LOAD_COMPONENT_SINGLE, STATUS_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, STATUS_UNSUPPORTED,
};

static PLUGIN_NAME: &[u8] = b"ExampleCar image loader\0";

static API: FileLoaderPluginV1 = FileLoaderPluginV1 {
    abi_version: ABI_VERSION_V1,
    struct_size: std::mem::size_of::<FileLoaderPluginV1>(),
    plugin_name,
    can_load,
    load,
};

unsafe extern "C" fn plugin_name() -> *const c_char {
    PLUGIN_NAME.as_ptr().cast::<c_char>()
}

unsafe extern "C" fn can_load(path: *const c_char, component: u32) -> u8 {
    std::panic::catch_unwind(|| {
        if component != LOAD_COMPONENT_SINGLE || path.is_null() {
            return 0;
        }
        // SAFETY: the host provides a NUL-terminated path for this call.
        let path = unsafe { CStr::from_ptr(path) }.to_string_lossy();
        supports_path(Path::new(path.as_ref())) as u8
    })
    .unwrap_or(0)
}

unsafe extern "C" fn load(
    path: *const c_char,
    base_address: u32,
    component: u32,
    sink: *const SegmentSinkV1,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if path.is_null() || sink.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: both pointers are valid for this synchronous ABI call.
        let (path, sink) = unsafe { (CStr::from_ptr(path).to_string_lossy().into_owned(), &*sink) };
        if sink.abi_version != ABI_VERSION_V1 {
            return STATUS_INVALID_ARGUMENT;
        }
        if component != LOAD_COMPONENT_SINGLE {
            set_error(sink, "this example loader handles separate images only");
            return STATUS_UNSUPPORTED;
        }
        let path = Path::new(&path);
        match load_inner(path, base_address, sink) {
            Ok(()) => STATUS_OK,
            Err(error) => {
                set_error(sink, &error);
                STATUS_ERROR
            }
        }
    }))
    .unwrap_or(STATUS_ERROR)
}

fn load_inner(path: &Path, base_address: u32, sink: &SegmentSinkV1) -> Result<(), String> {
    if !supports_path(path) {
        return Err(format!("unsupported image format: {}", path.display()));
    }
    let extension = extension(path);
    if extension == "bin" {
        let data = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if data.is_empty() {
            return Err(format!("{} is empty", path.display()));
        }
        return push(sink, base_address, &data);
    }

    let data_file = DataFile::open(path, MemorySegmentList::new(), 0, None)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut count = 0usize;
    for segment in data_file.base().segment_list.iter() {
        if !segment.is_initialized() || segment.data().is_empty() {
            continue;
        }
        let address = u32::try_from(segment.address)
            .map_err(|_| format!("{} contains an address above 0xFFFFFFFF", path.display()))?;
        push(sink, address, segment.data())?;
        count += 1;
    }
    if count == 0 {
        return Err(format!("{} contains no initialized data", path.display()));
    }
    Ok(())
}

fn push(sink: &SegmentSinkV1, address: u32, data: &[u8]) -> Result<(), String> {
    // SAFETY: the host owns the callback and copies `data` before returning.
    let status = unsafe { (sink.push_segment)(sink.user_data, address, data.as_ptr(), data.len()) };
    if status == STATUS_OK {
        Ok(())
    } else {
        Err(format!(
            "host rejected image segment at 0x{address:08X} with status {status}"
        ))
    }
}

fn set_error(sink: &SegmentSinkV1, message: &str) {
    let sanitized = message.replace('\0', "?");
    if let Ok(message) = CString::new(sanitized) {
        // SAFETY: the CString stays alive through the synchronous callback.
        unsafe { (sink.set_error)(sink.user_data, message.as_ptr()) };
    }
}

fn supports_path(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "hex" | "s19" | "s28" | "s37" | "srec" | "mot" | "bin"
    )
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

#[no_mangle]
pub extern "C" fn autors_security_file_loader_v1() -> *const FileLoaderPluginV1 {
    debug_assert_eq!(API.struct_size, file_loader_struct_size());
    &API
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_extensions_are_deliberately_bounded() {
        assert!(supports_path(Path::new("app.hex")));
        assert!(supports_path(Path::new("app.s19")));
        assert!(supports_path(Path::new("app.bin")));
        assert!(!supports_path(Path::new("archive.zip")));
    }
}
