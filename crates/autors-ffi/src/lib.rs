//! autors-ffi: C ABI wrapper over autors-a2l / autors-values.
//! handle pattern:
//! - [`AutorsProject`] wraps [`autors_a2l::Project`] (a `Box` whose ownership
//!   crosses the FFI boundary);
//! - [`AutorsMeasurement`] / [`AutorsCharacteristic`] are **borrowed handles**
//!   (they point at nodes inside the project; they become invalid once the
//!   project is freed or its structure is modified, own nothing, and must not
//!   be freed individually).
//!
//! Conventions:
//! - Every entry point is wrapped in `catch_unwind` so panics never cross the
//!   FFI boundary;
//! - Return code `0` = success, non-zero = error code (see the `AUTORS_ERR_*`
//!   constants); error details are retrieved via [`autors_last_error`]
//!   (thread-local, overwritten by the next FFI call);
//! - Strings returned as `*mut c_char` must be freed by the caller with
//!   [`autors_string_free`]; the pointer returned by [`autors_last_error`]
//!   must **not** be freed.
//!
//! `unsafe` appears only at the FFI boundary (handle reconstruction, C string
//! access, `Box` ownership transfer), and each site carries a `// SAFETY:`
//! comment.

// Exported safe fns must dereference raw pointer parameters — that is the
// established shape of a C ABI; null-pointer defense happens inside the
// function bodies, so the lint is allowed crate-wide.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::ptr;

use autors_a2l::model::characteristic::Characteristic;
use autors_a2l::model::enums::{A2lKeyword, DataType};
use autors_a2l::model::measurement::Measurement;
use autors_a2l::model::module::ModuleChild;
use autors_a2l::{Module, Project, ProjectChild};
use autors_values::value::{CompuTabRef, Conversion};

// ===========================================================================
// Error model
// ===========================================================================

/// Return code: success.
pub const AUTORS_OK: i32 = 0;
/// Return code: invalid argument (null pointer, malformed string, address out
/// of range, etc.).
pub const AUTORS_ERR_INVALID_ARG: i32 = 1;
/// Return code: lookup by name found nothing.
pub const AUTORS_ERR_NOT_FOUND: i32 = 2;
/// Return code: A2L parse / file read error.
pub const AUTORS_ERR_PARSE: i32 = 3;
/// Return code: internal error (write-out failure, conversion failure, caught
/// panic, etc.).
pub const AUTORS_ERR_INTERNAL: i32 = 4;

/// Return value of address queries: the measurement/characteristic has no
/// address set (the 32-bit all-ones sentinel, i.e. `u32::MAX`).
pub const AUTORS_ADDRESS_UNSET: u64 = 0xFFFF_FFFF;
/// Return value of address queries: the handle itself is invalid (details are
/// available via `autors_last_error` in this case).
pub const AUTORS_ADDRESS_INVALID: u64 = 0xFFFF_FFFF_FFFF_FFFF;

/// Internal error type (never crosses the boundary; only a return code plus a
/// thread-local error string do).
#[derive(Debug)]
enum FfiError {
    InvalidArg(String),
    NotFound(String),
    Parse(String),
    Internal(String),
}

impl FfiError {
    fn code(&self) -> i32 {
        match self {
            FfiError::InvalidArg(_) => AUTORS_ERR_INVALID_ARG,
            FfiError::NotFound(_) => AUTORS_ERR_NOT_FOUND,
            FfiError::Parse(_) => AUTORS_ERR_PARSE,
            FfiError::Internal(_) => AUTORS_ERR_INTERNAL,
        }
    }
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::InvalidArg(m) => write!(f, "invalid argument: {m}"),
            FfiError::NotFound(m) => write!(f, "not found: {m}"),
            FfiError::Parse(m) => write!(f, "parse error: {m}"),
            FfiError::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

thread_local! {
    /// Thread-local last-error string (`autors_last_error` returns a pointer
    /// into it).
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_last_error(msg: &str) {
    LAST_ERROR.with(|e| {
        // Error strings never contain an interior NUL; fall back to a default
        // empty string defensively.
        *e.borrow_mut() = CString::new(msg).unwrap_or_default();
    });
}

// ===========================================================================
// catch_unwind guards
// ===========================================================================

/// Guard for entry points without a return value (i32 return code).
fn guard_i32(f: impl FnOnce() -> Result<(), FfiError>) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => AUTORS_OK,
        Ok(Err(e)) => {
            set_last_error(&e.to_string());
            e.code()
        }
        Err(_) => {
            set_last_error("internal panic caught at FFI boundary");
            AUTORS_ERR_INTERNAL
        }
    }
}

/// Guard for entry points returning a raw pointer (NULL on failure).
fn guard_ptr<T>(f: impl FnOnce() -> Result<*mut T, FfiError>) -> *mut T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            set_last_error(&e.to_string());
            ptr::null_mut()
        }
        Err(_) => {
            set_last_error("internal panic caught at FFI boundary");
            ptr::null_mut()
        }
    }
}

/// Guard for entry points returning a string (NULL on failure).
fn guard_cstr(f: impl FnOnce() -> Result<CString, FfiError>) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(s)) => s.into_raw(),
        Ok(Err(e)) => {
            set_last_error(&e.to_string());
            ptr::null_mut()
        }
        Err(_) => {
            set_last_error("internal panic caught at FFI boundary");
            ptr::null_mut()
        }
    }
}

/// Guard for entry points returning a scalar (returns the `on_error` sentinel
/// and sets the error string on failure).
fn guard_val<T>(f: impl FnOnce() -> Result<T, FfiError>, on_error: T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_last_error(&e.to_string());
            on_error
        }
        Err(_) => {
            set_last_error("internal panic caught at FFI boundary");
            on_error
        }
    }
}

// ===========================================================================
// Opaque handle types
// ===========================================================================

/// Opaque A2L project handle. Created by `autors_project_new` /
/// `autors_project_parse_*`, freed with `autors_project_free`.
/// cbindgen:ignore
#[repr(C)]
pub struct AutorsProject {
    _private: [u8; 0],
}

/// Opaque measurement handle (borrowed: points at a node inside the project,
/// owns nothing).
/// cbindgen:ignore
#[repr(C)]
pub struct AutorsMeasurement {
    _private: [u8; 0],
}

/// Opaque characteristic handle (borrowed: points at a node inside the
/// project, owns nothing).
/// cbindgen:ignore
#[repr(C)]
pub struct AutorsCharacteristic {
    _private: [u8; 0],
}

fn project_into_handle(p: Project) -> *mut AutorsProject {
    Box::into_raw(Box::new(p)) as *mut AutorsProject
}

/// Reconstruct a read-only project reference; null pointer is an error.
fn project_ref<'a>(handle: *const AutorsProject) -> Result<&'a Project, FfiError> {
    if handle.is_null() {
        return Err(FfiError::InvalidArg("project handle is null".into()));
    }
    // SAFETY: the handle can only come from Box::into_raw in
    // autors_project_new/parse_*, and the caller guarantees it is not used
    // after autors_project_free (contract on the C side).
    Ok(unsafe { &*(handle as *const Project) })
}

/// Reconstruct a mutable project reference; null pointer is an error.
fn project_mut<'a>(handle: *mut AutorsProject) -> Result<&'a mut Project, FfiError> {
    if handle.is_null() {
        return Err(FfiError::InvalidArg("project handle is null".into()));
    }
    // SAFETY: same as project_ref; the caller guarantees no aliasing handle
    // is used concurrently for the duration of the access.
    Ok(unsafe { &mut *(handle as *mut Project) })
}

/// Reconstruct a read-only measurement reference; null pointer is an error.
fn meas_ref<'a>(handle: *const AutorsMeasurement) -> Result<&'a Measurement, FfiError> {
    if handle.is_null() {
        return Err(FfiError::InvalidArg("measurement handle is null".into()));
    }
    // SAFETY: the borrowed handle comes from autors_project_find_measurement
    // and points at a node inside a live project; the FFI side only reads, so
    // no mutable alias is created.
    Ok(unsafe { &*(handle as *const Measurement) })
}

/// Reconstruct a read-only characteristic reference; null pointer is an error.
fn char_ref<'a>(handle: *const AutorsCharacteristic) -> Result<&'a Characteristic, FfiError> {
    if handle.is_null() {
        return Err(FfiError::InvalidArg("characteristic handle is null".into()));
    }
    // SAFETY: same as meas_ref.
    Ok(unsafe { &*(handle as *const Characteristic) })
}

/// Read a C string argument as &str; null pointer / non-UTF-8 is an error.
fn cstr_arg<'a>(p: *const c_char, what: &str) -> Result<&'a str, FfiError> {
    if p.is_null() {
        return Err(FfiError::InvalidArg(format!("{what} is null")));
    }
    // SAFETY: the caller guarantees a valid NUL-terminated C string (contract
    // on the C side).
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| FfiError::InvalidArg(format!("{what} is not valid UTF-8")))
}

/// Rust String → heap C string (the caller is responsible for
/// autors_string_free).
fn into_c_string(s: impl Into<String>) -> Result<CString, FfiError> {
    CString::new(s.into()).map_err(|_| FfiError::Internal("string contains NUL byte".into()))
}

// ===========================================================================
// Model lookup helpers
// ===========================================================================

/// Find a measurement by name across all modules; returns the containing
/// module and the node.
fn find_measurement<'a>(p: &'a Project, name: &str) -> Option<(&'a Module, &'a Measurement)> {
    p.modules().find_map(|m| {
        m.children.iter().find_map(|c| match c {
            ModuleChild::Measurement(meas) if meas.named.name == name => Some((m, meas)),
            _ => None,
        })
    })
}

/// Find a characteristic by name across all modules.
fn find_characteristic<'a>(p: &'a Project, name: &str) -> Option<&'a Characteristic> {
    p.modules()
        .flat_map(|m| m.children.iter())
        .find_map(|c| match c {
            ModuleChild::Characteristic(ch) if ch.named.name == name => Some(ch),
            _ => None,
        })
}

/// Build a conversion context within a module by name (COMPU_METHOD plus the
/// resolved COMPU_TAB reference).
fn build_conversion<'a>(module: &'a Module, conv_name: &str) -> Result<Conversion<'a>, FfiError> {
    let cm = module
        .children
        .iter()
        .find_map(|c| match c {
            ModuleChild::CompuMethod(m) if m.name == conv_name => Some(m),
            _ => None,
        })
        .ok_or_else(|| {
            FfiError::NotFound(format!(
                "COMPU_METHOD '{conv_name}' not found in module '{}'",
                module.name
            ))
        })?;
    let tab = cm.compu_tab_ref.as_deref().and_then(|tref| {
        module.children.iter().find_map(|c| match c {
            ModuleChild::CompuTab(t) if t.base.name == tref => Some(CompuTabRef::Tab(t)),
            ModuleChild::CompuVtab(t) if t.base.name == tref => Some(CompuTabRef::Vtab(t)),
            ModuleChild::CompuVtabRange(t) if t.base.name == tref => {
                Some(CompuTabRef::VtabRange(t))
            }
            _ => None,
        })
    });
    Ok(match tab {
        Some(t) => Conversion::with_tab(cm, t),
        None => Conversion::new(cm),
    })
}

// ===========================================================================
// Lifecycle
// ===========================================================================

/// Create an empty project. Returns NULL on failure (see `autors_last_error`
/// for details).
#[no_mangle]
pub extern "C" fn autors_project_new() -> *mut AutorsProject {
    guard_ptr(|| Ok(project_into_handle(Project::default())))
}

/// Free a project handle (NULL-safe). All borrowed handles
/// (measurement/characteristic) become invalid after this call.
#[no_mangle]
pub extern "C" fn autors_project_free(handle: *mut AutorsProject) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !handle.is_null() {
            // SAFETY: the handle comes from Box::into_raw in
            // project_into_handle; ownership transfers back into a Box here
            // and is dropped, and the caller guarantees it is freed only once.
            unsafe { drop(Box::from_raw(handle as *mut Project)) };
        }
    }));
}

/// Free a string returned by an `autors_*` function (NULL-safe). Must not be
/// used on the return value of `autors_last_error`.
#[no_mangle]
pub extern "C" fn autors_string_free(s: *mut c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !s.is_null() {
            // SAFETY: the pointer comes from CString::into_raw in this crate;
            // ownership transfers back here and is dropped.
            unsafe { drop(CString::from_raw(s)) };
        }
    }));
}

/// Get details of the last FFI error on the current thread (an empty string
/// when there is no error).
/// The returned pointer refers to thread-local storage: **do not free it**;
/// it stays valid until the next FFI call.
#[no_mangle]
pub extern "C" fn autors_last_error() -> *const c_char {
    catch_unwind(|| LAST_ERROR.with(|e| e.borrow().as_ptr())).unwrap_or(ptr::null())
}

// ===========================================================================
// Parse / write out
// ===========================================================================

/// Parse an A2L project from a file. Returns NULL on failure.
#[no_mangle]
pub extern "C" fn autors_project_parse_file(path: *const c_char) -> *mut AutorsProject {
    guard_ptr(|| {
        let path = cstr_arg(path, "path")?;
        let p = Project::parse_file(Path::new(path)).map_err(|e| FfiError::Parse(e.to_string()))?;
        Ok(project_into_handle(p))
    })
}

/// Parse an A2L project from a text string. Returns NULL on failure.
#[no_mangle]
pub extern "C" fn autors_project_parse_string(text: *const c_char) -> *mut AutorsProject {
    guard_ptr(|| {
        let text = cstr_arg(text, "text")?;
        let p = Project::parse_str(text).map_err(|e| FfiError::Parse(e.to_string()))?;
        Ok(project_into_handle(p))
    })
}

/// Write the project out as A2L text. The return value must be freed with
/// `autors_string_free`; NULL on failure.
#[no_mangle]
pub extern "C" fn autors_project_write_string(handle: *const AutorsProject) -> *mut c_char {
    guard_cstr(|| {
        let p = project_ref(handle)?;
        let out = p
            .write_string()
            .map_err(|e| FfiError::Internal(e.to_string()))?;
        into_c_string(out)
    })
}

/// Save the project to a file. Returns 0 on success.
#[no_mangle]
pub extern "C" fn autors_project_save(handle: *const AutorsProject, path: *const c_char) -> i32 {
    guard_i32(|| {
        let p = project_ref(handle)?;
        let path = cstr_arg(path, "path")?;
        p.save(Path::new(path))
            .map_err(|e| FfiError::Internal(e.to_string()))
    })
}

// ===========================================================================
// Queries: module / measurement / characteristic
// ===========================================================================

/// Number of MODULEs in the project. Returns -1 if the handle is invalid.
#[no_mangle]
pub extern "C" fn autors_project_module_count(handle: *const AutorsProject) -> i32 {
    guard_val(
        || {
            let p = project_ref(handle)?;
            i32::try_from(p.modules().count())
                .map_err(|_| FfiError::Internal("module count overflows i32".into()))
        },
        -1,
    )
}

/// Name of the MODULE at position `index`. The return value must be freed
/// with `autors_string_free`; NULL if out of range or on failure.
#[no_mangle]
pub extern "C" fn autors_project_module_name(
    handle: *const AutorsProject,
    index: i32,
) -> *mut c_char {
    guard_cstr(|| {
        if index < 0 {
            return Err(FfiError::InvalidArg("index is negative".into()));
        }
        let p = project_ref(handle)?;
        let m = p
            .modules()
            .nth(index as usize)
            .ok_or_else(|| FfiError::NotFound(format!("module index {index} out of range")))?;
        into_c_string(m.name.clone())
    })
}

/// Total number of MEASUREMENTs across all MODULEs. Returns -1 if the handle
/// is invalid.
#[no_mangle]
pub extern "C" fn autors_project_measurement_count(handle: *const AutorsProject) -> i32 {
    guard_val(
        || {
            let p = project_ref(handle)?;
            let n = p
                .modules()
                .flat_map(|m| m.children.iter())
                .filter(|c| matches!(c, ModuleChild::Measurement(_)))
                .count();
            i32::try_from(n).map_err(|_| FfiError::Internal("count overflows i32".into()))
        },
        -1,
    )
}

/// Find a measurement by name across all modules. Returns a borrowed handle
/// (lives as long as the project; do not free it individually);
/// NULL if not found.
#[no_mangle]
pub extern "C" fn autors_project_find_measurement(
    handle: *const AutorsProject,
    name: *const c_char,
) -> *mut AutorsMeasurement {
    guard_ptr(|| {
        let p = project_ref(handle)?;
        let name = cstr_arg(name, "name")?;
        let (_, meas) = find_measurement(p, name)
            .ok_or_else(|| FfiError::NotFound(format!("measurement '{name}' not found")))?;
        // Borrowed handle: the FFI side performs no mutable access (see the
        // module-level documentation).
        Ok(meas as *const Measurement as *mut AutorsMeasurement)
    })
}

/// Find a characteristic by name across all modules. Returns a borrowed
/// handle (lives as long as the project; do not free it individually);
/// NULL if not found.
#[no_mangle]
pub extern "C" fn autors_project_find_characteristic(
    handle: *const AutorsProject,
    name: *const c_char,
) -> *mut AutorsCharacteristic {
    guard_ptr(|| {
        let p = project_ref(handle)?;
        let name = cstr_arg(name, "name")?;
        let ch = find_characteristic(p, name)
            .ok_or_else(|| FfiError::NotFound(format!("characteristic '{name}' not found")))?;
        Ok(ch as *const Characteristic as *mut AutorsCharacteristic)
    })
}

/// Measurement name. The return value must be freed with
/// `autors_string_free`; NULL on failure.
#[no_mangle]
pub extern "C" fn autors_measurement_name(meas: *const AutorsMeasurement) -> *mut c_char {
    guard_cstr(|| into_c_string(meas_ref(meas)?.named.name.clone()))
}

/// Measurement long description (LONG_IDENT). The return value must be freed
/// with `autors_string_free`; NULL on failure.
#[no_mangle]
pub extern "C" fn autors_measurement_long_identifier(
    meas: *const AutorsMeasurement,
) -> *mut c_char {
    guard_cstr(|| {
        into_c_string(
            meas_ref(meas)?
                .named
                .description
                .clone()
                .unwrap_or_default(),
        )
    })
}

/// Measurement data type keyword (e.g. "UWORD"; "Unsupported" for types with
/// no keyword). The return value must be freed with `autors_string_free`;
/// NULL on failure.
#[no_mangle]
pub extern "C" fn autors_measurement_data_type(meas: *const AutorsMeasurement) -> *mut c_char {
    guard_cstr(|| {
        let dt = meas_ref(meas)?.data_type;
        into_c_string(dt.as_keyword().unwrap_or("Unsupported"))
    })
}

/// Name of the COMPU_METHOD referenced by the measurement. The return value
/// must be freed with `autors_string_free`; NULL on failure.
#[no_mangle]
pub extern "C" fn autors_measurement_conversion_name(
    meas: *const AutorsMeasurement,
) -> *mut c_char {
    guard_cstr(|| into_c_string(meas_ref(meas)?.conv.conversion.clone()))
}

/// Measurement address (ECU_ADDRESS). Returns `AUTORS_ADDRESS_UNSET` if no
/// address is set; `AUTORS_ADDRESS_INVALID` if the handle is invalid.
#[no_mangle]
pub extern "C" fn autors_measurement_address(meas: *const AutorsMeasurement) -> u64 {
    guard_val(
        || {
            Ok(meas_ref(meas)?
                .addr
                .address
                .map(u64::from)
                .unwrap_or(AUTORS_ADDRESS_UNSET))
        },
        AUTORS_ADDRESS_INVALID,
    )
}

/// Characteristic name. The return value must be freed with
/// `autors_string_free`; NULL on failure.
#[no_mangle]
pub extern "C" fn autors_characteristic_name(ch: *const AutorsCharacteristic) -> *mut c_char {
    guard_cstr(|| into_c_string(char_ref(ch)?.named.name.clone()))
}

/// Characteristic address. Returns `AUTORS_ADDRESS_UNSET` if no address is
/// set; `AUTORS_ADDRESS_INVALID` if the handle is invalid.
#[no_mangle]
pub extern "C" fn autors_characteristic_address(ch: *const AutorsCharacteristic) -> u64 {
    guard_val(
        || {
            Ok(char_ref(ch)?
                .addr
                .address
                .map(u64::from)
                .unwrap_or(AUTORS_ADDRESS_UNSET))
        },
        AUTORS_ADDRESS_INVALID,
    )
}

/// Name of the RECORD_LAYOUT referenced by the characteristic. The return
/// value must be freed with `autors_string_free`; NULL on failure.
#[no_mangle]
pub extern "C" fn autors_characteristic_record_layout(
    ch: *const AutorsCharacteristic,
) -> *mut c_char {
    guard_cstr(|| into_c_string(char_ref(ch)?.rec.record_layout.clone()))
}

/// Name of the COMPU_METHOD referenced by the characteristic. The return
/// value must be freed with `autors_string_free`; NULL on failure.
#[no_mangle]
pub extern "C" fn autors_characteristic_conversion_name(
    ch: *const AutorsCharacteristic,
) -> *mut c_char {
    guard_cstr(|| into_c_string(char_ref(ch)?.conv.conversion.clone()))
}

// ===========================================================================
// Conversion (autors-values Conversion)
// ===========================================================================

/// Raw value → physical value (using the COMPU_METHOD referenced by the
/// measurement; COMPU_TAB_REF is resolved within the same module).
/// Returns 0 on success and writes `*out`.
#[no_mangle]
pub extern "C" fn autors_measurement_to_physical(
    handle: *const AutorsProject,
    name: *const c_char,
    raw: f64,
    out: *mut f64,
) -> i32 {
    guard_i32(|| {
        if out.is_null() {
            return Err(FfiError::InvalidArg("out is null".into()));
        }
        let p = project_ref(handle)?;
        let name = cstr_arg(name, "name")?;
        let (module, meas) = find_measurement(p, name)
            .ok_or_else(|| FfiError::NotFound(format!("measurement '{name}' not found")))?;
        let conv = build_conversion(module, &meas.conv.conversion)?;
        let v = conv
            .to_physical(raw)
            .map_err(|e| FfiError::Internal(e.to_string()))?;
        // SAFETY: out was checked non-null; the caller guarantees it points
        // to a writable f64.
        unsafe { *out = v };
        Ok(())
    })
}

/// Physical value → raw value (rounded/truncated according to the
/// measurement's data type). Returns 0 on success and writes `*out`.
#[no_mangle]
pub extern "C" fn autors_measurement_to_raw(
    handle: *const AutorsProject,
    name: *const c_char,
    physical: f64,
    out: *mut f64,
) -> i32 {
    guard_i32(|| {
        if out.is_null() {
            return Err(FfiError::InvalidArg("out is null".into()));
        }
        let p = project_ref(handle)?;
        let name = cstr_arg(name, "name")?;
        let (module, meas) = find_measurement(p, name)
            .ok_or_else(|| FfiError::NotFound(format!("measurement '{name}' not found")))?;
        let conv = build_conversion(module, &meas.conv.conversion)?;
        let dt: DataType = meas.data_type;
        let v = conv
            .to_raw(dt, physical)
            .map_err(|e| FfiError::Internal(e.to_string()))?;
        // SAFETY: out was checked non-null; the caller guarantees it points
        // to a writable f64.
        unsafe { *out = v };
        Ok(())
    })
}

// ===========================================================================
// Updates
// ===========================================================================

/// Update a measurement's address by name (ECU_ADDRESS; the address must fit
/// in 32 bits). Returns 0 on success.
#[no_mangle]
pub extern "C" fn autors_measurement_set_address(
    handle: *mut AutorsProject,
    name: *const c_char,
    addr: u64,
) -> i32 {
    guard_i32(|| {
        let name = cstr_arg(name, "name")?;
        if addr > u64::from(u32::MAX) {
            return Err(FfiError::InvalidArg(format!(
                "address 0x{addr:X} exceeds 32-bit range"
            )));
        }
        let p = project_mut(handle)?;
        let meas = p
            .children
            .iter_mut()
            .filter_map(|c| match c {
                ProjectChild::Module(m) => Some(m),
                _ => None,
            })
            .flat_map(|m| m.children.iter_mut())
            .find_map(|c| match c {
                ModuleChild::Measurement(meas) if meas.named.name == name => Some(meas),
                _ => None,
            })
            .ok_or_else(|| FfiError::NotFound(format!("measurement '{name}' not found")))?;
        meas.addr.address = Some(addr as u32);
        Ok(())
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    const SAMPLE: &str = r#"/begin PROJECT Demo "demo project"
/begin MODULE M1 "module one"
/begin COMPU_METHOD Conv_EngineSpeed "rpm conv" LINEAR "%4.0" "rpm"
COEFFS_LINEAR 2 3
/end COMPU_METHOD
/begin MEASUREMENT EngineSpeed "engine speed" UWORD Conv_EngineSpeed 1 0 0 10000 ECU_ADDRESS 0x1000
/end MEASUREMENT
/begin CHARACTERISTIC KFactor "factor" VALUE 0x4000 RL_DEFAULT 0 Conv_EngineSpeed 0 100
/end CHARACTERISTIC
/end MODULE
/end PROJECT
"#;

    fn parse_sample() -> *mut AutorsProject {
        let text = CString::new(SAMPLE).unwrap();
        let h = autors_project_parse_string(text.as_ptr());
        assert!(!h.is_null());
        h
    }

    /// Take ownership of a returned string and free it.
    unsafe fn take_string(p: *mut c_char) -> String {
        assert!(!p.is_null());
        // SAFETY: p comes from CString::into_raw on the FFI side and is
        // non-null and NUL-terminated.
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        autors_string_free(p);
        s
    }

    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    #[test]
    fn lifecycle_parse_query_convert_update_write() {
        let proj = parse_sample();
        assert_eq!(autors_project_module_count(proj), 1);
        assert_eq!(autors_project_measurement_count(proj), 1);
        assert_eq!(
            unsafe { take_string(autors_project_module_name(proj, 0)) },
            "M1"
        );

        let name = c("EngineSpeed");
        let meas = autors_project_find_measurement(proj, name.as_ptr());
        assert!(!meas.is_null());
        assert_eq!(
            unsafe { take_string(autors_measurement_name(meas)) },
            "EngineSpeed"
        );
        assert_eq!(
            unsafe { take_string(autors_measurement_long_identifier(meas)) },
            "engine speed"
        );
        assert_eq!(
            unsafe { take_string(autors_measurement_data_type(meas)) },
            "UWORD"
        );
        assert_eq!(
            unsafe { take_string(autors_measurement_conversion_name(meas)) },
            "Conv_EngineSpeed"
        );
        assert_eq!(autors_measurement_address(meas), 0x1000);

        let ch_name = c("KFactor");
        let ch = autors_project_find_characteristic(proj, ch_name.as_ptr());
        assert!(!ch.is_null());
        assert_eq!(
            unsafe { take_string(autors_characteristic_name(ch)) },
            "KFactor"
        );
        assert_eq!(autors_characteristic_address(ch), 0x4000);
        assert_eq!(
            unsafe { take_string(autors_characteristic_record_layout(ch)) },
            "RL_DEFAULT"
        );
        assert_eq!(
            unsafe { take_string(autors_characteristic_conversion_name(ch)) },
            "Conv_EngineSpeed"
        );

        // Conversion: phys = raw * 2 + 3
        let mut phys = 0.0f64;
        assert_eq!(
            autors_measurement_to_physical(proj, name.as_ptr(), 10.0, &mut phys),
            AUTORS_OK
        );
        assert_eq!(phys, 23.0);
        let mut raw = 0.0f64;
        assert_eq!(
            autors_measurement_to_raw(proj, name.as_ptr(), phys, &mut raw),
            AUTORS_OK
        );
        assert_eq!(raw, 10.0);

        // Update the address and verify the write-out
        assert_eq!(
            autors_measurement_set_address(proj, name.as_ptr(), 0x2000),
            AUTORS_OK
        );
        let meas2 = autors_project_find_measurement(proj, name.as_ptr());
        assert_eq!(autors_measurement_address(meas2), 0x2000);

        let out = unsafe { take_string(autors_project_write_string(proj)) };
        assert!(out.contains("/begin PROJECT"));
        assert!(out.contains("/begin MEASUREMENT EngineSpeed"));
        assert!(out.contains("ECU_ADDRESS 0x2000"));

        autors_project_free(proj);
        autors_project_free(ptr::null_mut()); // NULL-safe
        autors_string_free(ptr::null_mut());
    }

    #[test]
    fn save_and_parse_file_roundtrip() {
        let proj = parse_sample();
        let path = std::env::temp_dir().join("autors_ffi_save_test.a2l");
        let cpath = CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(autors_project_save(proj, cpath.as_ptr()), AUTORS_OK);
        let proj2 = autors_project_parse_file(cpath.as_ptr());
        assert!(!proj2.is_null());
        assert_eq!(autors_project_measurement_count(proj2), 1);
        autors_project_free(proj2);
        autors_project_free(proj);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn error_paths_set_last_error() {
        // Parse failure → NULL + error string
        let bad = c("not an a2l");
        let h = autors_project_parse_string(bad.as_ptr());
        assert!(h.is_null());
        let err = unsafe { CStr::from_ptr(autors_last_error()) }
            .to_string_lossy()
            .into_owned();
        assert!(!err.is_empty());

        // Null project handle → INVALID_ARG
        let name = c("X");
        assert!(autors_project_find_measurement(ptr::null(), name.as_ptr()).is_null());
        let mut out = 0.0f64;
        assert_eq!(
            autors_measurement_to_physical(ptr::null(), name.as_ptr(), 1.0, &mut out),
            AUTORS_ERR_INVALID_ARG
        );

        // Not found → NULL / NOT_FOUND
        let proj = parse_sample();
        let missing = c("NoSuchMeas");
        assert!(autors_project_find_measurement(proj, missing.as_ptr()).is_null());
        assert_eq!(
            autors_measurement_set_address(proj, missing.as_ptr(), 0x10),
            AUTORS_ERR_NOT_FOUND
        );
        assert!(autors_project_module_name(proj, 7).is_null());

        // Address out of range → INVALID_ARG
        let eng = c("EngineSpeed");
        assert_eq!(
            autors_measurement_set_address(proj, eng.as_ptr(), u64::from(u32::MAX) + 1),
            AUTORS_ERR_INVALID_ARG
        );

        // Null out pointer → INVALID_ARG
        assert_eq!(
            autors_measurement_to_physical(proj, eng.as_ptr(), 1.0, ptr::null_mut()),
            AUTORS_ERR_INVALID_ARG
        );

        // Null measurement handle → sentinel values
        assert_eq!(
            autors_measurement_address(ptr::null()),
            AUTORS_ADDRESS_INVALID
        );
        assert!(autors_measurement_name(ptr::null()).is_null());
        autors_project_free(proj);
    }
}
