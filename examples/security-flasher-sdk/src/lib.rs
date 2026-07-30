//! Versioned C ABI for Security Flasher example plugins.
//!
//! Only C-compatible values cross the dynamic-library boundary. Plugins must
//! not retain pointers supplied by the host after the callback returns.

use std::ffi::{c_char, c_void};

pub const ABI_VERSION_V1: u32 = 1;

pub const STATUS_OK: i32 = 0;
pub const STATUS_ERROR: i32 = 1;
pub const STATUS_INVALID_ARGUMENT: i32 = 2;
pub const STATUS_BUFFER_TOO_SMALL: i32 = 3;
pub const STATUS_CANCELLED: i32 = 4;
pub const STATUS_UNSUPPORTED: i32 = 5;

pub const LOADER_MODE_SEPARATE: u32 = 0;
pub const LOADER_MODE_PACKAGE: u32 = 1;

pub const LOAD_COMPONENT_SINGLE: u32 = 0;
pub const LOAD_COMPONENT_PACKAGE_DRIVER: u32 = 1;
pub const LOAD_COMPONENT_PACKAGE_APPLICATION: u32 = 2;

pub const FLOW_ENTRY_SYMBOL: &[u8] = b"autors_security_flow_v1\0";
pub const FILE_LOADER_ENTRY_SYMBOL: &[u8] = b"autors_security_file_loader_v1\0";

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FlowPluginV1 {
    pub abi_version: u32,
    pub struct_size: usize,
    pub plugin_name: unsafe extern "C" fn() -> *const c_char,
    pub execute: unsafe extern "C" fn(context: *const FlowContextV1) -> i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileLoaderPluginV1 {
    pub abi_version: u32,
    pub struct_size: usize,
    pub plugin_name: unsafe extern "C" fn() -> *const c_char,
    pub can_load: unsafe extern "C" fn(path: *const c_char, component: u32) -> u8,
    pub load: unsafe extern "C" fn(
        path: *const c_char,
        base_address: u32,
        component: u32,
        sink: *const SegmentSinkV1,
    ) -> i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SegmentSinkV1 {
    pub abi_version: u32,
    pub user_data: *mut c_void,
    pub push_segment: unsafe extern "C" fn(
        user_data: *mut c_void,
        address: u32,
        data: *const u8,
        data_len: usize,
    ) -> i32,
    pub set_error: unsafe extern "C" fn(user_data: *mut c_void, message: *const c_char),
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FlowContextV1 {
    pub abi_version: u32,
    pub struct_size: usize,
    pub user_data: *mut c_void,
    pub application_path: *const c_char,
    pub flash_driver_path: *const c_char,
    pub flash_driver_address: u32,
    pub loader_mode: u32,
    pub transfer_block_size: usize,
    pub physical_request_id: u32,
    pub functional_request_id: u32,
    pub response_id: u32,
    pub report: unsafe extern "C" fn(user_data: *mut c_void, percent: i32, message: *const c_char),
    pub log: unsafe extern "C" fn(user_data: *mut c_void, message: *const c_char),
    pub set_error: unsafe extern "C" fn(user_data: *mut c_void, message: *const c_char),
    pub is_cancelled: unsafe extern "C" fn(user_data: *mut c_void) -> u8,
    pub uds_request: unsafe extern "C" fn(
        user_data: *mut c_void,
        request_id: u32,
        request: *const u8,
        request_len: usize,
        await_response: u8,
        response: *mut u8,
        response_capacity: usize,
        response_len: *mut usize,
    ) -> i32,
    pub compute_key: unsafe extern "C" fn(
        user_data: *mut c_void,
        security_level: u32,
        variant: *const u8,
        variant_len: usize,
        seed: *const u8,
        seed_len: usize,
        key: *mut u8,
        key_capacity: usize,
        key_len: *mut usize,
    ) -> i32,
    pub load_image: unsafe extern "C" fn(
        user_data: *mut c_void,
        path: *const c_char,
        base_address: u32,
        component: u32,
        image_handle: *mut u64,
    ) -> i32,
    pub image_segment_count:
        unsafe extern "C" fn(user_data: *mut c_void, image_handle: u64, count: *mut usize) -> i32,
    pub image_segment: unsafe extern "C" fn(
        user_data: *mut c_void,
        image_handle: u64,
        index: usize,
        address: *mut u32,
        data: *mut *const u8,
        data_len: *mut usize,
    ) -> i32,
    pub free_image: unsafe extern "C" fn(user_data: *mut c_void, image_handle: u64),
}

pub type FlowEntryV1 = unsafe extern "C" fn() -> *const FlowPluginV1;
pub type FileLoaderEntryV1 = unsafe extern "C" fn() -> *const FileLoaderPluginV1;

pub fn flow_struct_size() -> usize {
    std::mem::size_of::<FlowPluginV1>()
}

pub fn file_loader_struct_size() -> usize {
    std::mem::size_of::<FileLoaderPluginV1>()
}

pub fn flow_context_struct_size() -> usize {
    std::mem::size_of::<FlowContextV1>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_and_sizes_are_non_zero() {
        assert_eq!(ABI_VERSION_V1, 1);
        assert!(flow_struct_size() > 0);
        assert!(file_loader_struct_size() > 0);
        assert!(flow_context_struct_size() > 0);
    }
}
