use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use autors_diag::blocking::BlockingTransport;
use autors_diag::uds::MsgState;
use autors_diag::uds_transport::IsoTpTransport;
use autors_security_flasher_sdk::{
    flow_context_struct_size, FlowContextV1, ABI_VERSION_V1, LOADER_MODE_PACKAGE,
    LOADER_MODE_SEPARATE, STATUS_BUFFER_TOO_SMALL, STATUS_CANCELLED, STATUS_ERROR,
    STATUS_INVALID_ARGUMENT, STATUS_OK,
};

use crate::config::{LoaderMode, VehicleFlashConfig};
use crate::device::{wire_id, DynCanDevice};
use crate::error::{Error, Result};
use crate::plugins::{resolve_library_path, FlashImage, LoadedFileLoader, LoadedFlow};

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    Progress { percent: i32, message: String },
    Log(String),
}

pub type EventSink = Arc<dyn Fn(RuntimeEvent) + Send + Sync + 'static>;

pub fn execute_flow(
    device: DynCanDevice,
    config: &VehicleFlashConfig,
    application_path: &Path,
    cancelled: Arc<AtomicBool>,
    sink: EventSink,
) -> (DynCanDevice, Result<()>) {
    let prepared = prepare_plugins(config);
    let (flow, loader, seed_key, seed_path) = match prepared {
        Ok(plugins) => plugins,
        Err(error) => return (device, Err(error)),
    };

    (sink)(RuntimeEvent::Log(format!(
        "Loaded flow '{}' from {}",
        flow.name,
        flow.path.display()
    )));
    (sink)(RuntimeEvent::Log(format!(
        "Loaded file loader '{}' from {}",
        loader.name,
        loader.path.display()
    )));
    (sink)(RuntimeEvent::Log(format!(
        "Loaded Seed & Key provider from {}",
        seed_path.display()
    )));

    let mut transport = IsoTpTransport::new(
        device,
        wire_id(config.can.physical_request_id),
        wire_id(config.can.response_id),
        config.can.data_baud_rate != 0,
    );
    transport.isotp.p2_client = config.can.p2_client_ms;
    transport.isotp.p3_client = config.can.p3_client_ms;
    transport.isotp.use_fill_byte = true;
    transport.isotp.fill_byte = config.can.fill_byte;

    let mut runtime = PluginRuntime {
        transport: BlockingTransport::new(transport),
        loader,
        seed_key,
        images: HashMap::new(),
        next_image_handle: 1,
        cancelled,
        sink,
        last_error: None,
    };

    let application_c = match path_to_c_string(application_path) {
        Ok(value) => value,
        Err(error) => {
            let device = runtime.transport.into_inner().device;
            return (device, Err(error));
        }
    };
    let driver_path = config.flash_driver_path();
    let driver_c = match driver_path.as_deref() {
        Some(path) => match path_to_c_string(path) {
            Ok(value) => value,
            Err(error) => {
                let device = runtime.transport.into_inner().device;
                return (device, Err(error));
            }
        },
        None => CString::default(),
    };

    let context = FlowContextV1 {
        abi_version: ABI_VERSION_V1,
        struct_size: flow_context_struct_size(),
        user_data: (&mut runtime as *mut PluginRuntime).cast::<c_void>(),
        application_path: application_c.as_ptr(),
        flash_driver_path: driver_c.as_ptr(),
        flash_driver_address: config
            .flash_driver
            .as_ref()
            .map_or(0, |driver| driver.address),
        loader_mode: match config.loader.mode {
            LoaderMode::Separate => LOADER_MODE_SEPARATE,
            LoaderMode::Package => LOADER_MODE_PACKAGE,
        },
        transfer_block_size: config.can.transfer_block_size,
        physical_request_id: config.can.physical_request_id,
        functional_request_id: config.can.functional_request_id,
        response_id: config.can.response_id,
        report: report_callback,
        log: log_callback,
        set_error: set_error_callback,
        is_cancelled: is_cancelled_callback,
        uds_request: uds_request_callback,
        compute_key: compute_key_callback,
        load_image: load_image_callback,
        image_segment_count: image_segment_count_callback,
        image_segment: image_segment_callback,
        free_image: free_image_callback,
    };
    let status = flow.execute(&context);
    let result = if status == STATUS_OK {
        Ok(())
    } else if status == STATUS_CANCELLED || runtime.cancelled.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Err(Error::Plugin(runtime.last_error.take().unwrap_or_else(
            || format!("flow '{}' failed with status {status}", flow.name),
        )))
    };
    let device = runtime.transport.into_inner().device;
    (device, result)
}

fn prepare_plugins(
    config: &VehicleFlashConfig,
) -> Result<(
    LoadedFlow,
    LoadedFileLoader,
    SeedKeyLibrary,
    std::path::PathBuf,
)> {
    let flow = LoadedFlow::load(&config.resolve(&config.flow.dll_path))?;
    let loader = LoadedFileLoader::load(&config.resolve(&config.loader.dll_path))?;
    let seed_path = resolve_library_path(&config.resolve(&config.seed_key.dll_path))?;
    let seed_key = SeedKeyLibrary::load(&seed_path)?;
    Ok((flow, loader, seed_key, seed_path))
}

struct PluginRuntime {
    transport: BlockingTransport<IsoTpTransport<DynCanDevice>>,
    loader: LoadedFileLoader,
    seed_key: SeedKeyLibrary,
    images: HashMap<u64, FlashImage>,
    next_image_handle: u64,
    cancelled: Arc<AtomicBool>,
    sink: EventSink,
    last_error: Option<String>,
}

impl PluginRuntime {
    fn fail(&mut self, message: impl Into<String>) -> i32 {
        self.last_error = Some(message.into());
        STATUS_ERROR
    }
}

#[cfg(windows)]
struct SeedKeyLibrary(autors_native::NativeSkDll);

#[cfg(windows)]
impl SeedKeyLibrary {
    fn load(path: &Path) -> Result<Self> {
        autors_native::NativeSkDll::new(path.to_string_lossy().into_owned())
            .map(Self)
            .map_err(|error| Error::Security(format!("{}: {error}", path.display())))
    }

    fn compute(&self, level: u32, variant: &[u8], seed: &[u8]) -> Result<Vec<u8>> {
        let (status, key) = self.0.compute_key_from_seed_uds(level, variant, seed);
        key.ok_or_else(|| Error::Security(format!("Seed & Key DLL returned {status:?}")))
    }
}

#[cfg(not(windows))]
struct SeedKeyLibrary;

#[cfg(not(windows))]
impl SeedKeyLibrary {
    fn load(path: &Path) -> Result<Self> {
        Err(Error::Security(format!(
            "native Seed & Key libraries are only supported on Windows: {}",
            path.display()
        )))
    }

    fn compute(&self, _level: u32, _variant: &[u8], _seed: &[u8]) -> Result<Vec<u8>> {
        Err(Error::Security(
            "native Seed & Key libraries are only supported on Windows".to_string(),
        ))
    }
}

unsafe extern "C" fn report_callback(user_data: *mut c_void, percent: i32, message: *const c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = runtime_from_ptr(user_data) else {
            return;
        };
        let text = copy_callback_string(message);
        (runtime.sink)(RuntimeEvent::Progress {
            percent: percent.clamp(0, 100),
            message: text.clone(),
        });
        (runtime.sink)(RuntimeEvent::Log(text));
    }));
}

unsafe extern "C" fn log_callback(user_data: *mut c_void, message: *const c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = runtime_from_ptr(user_data) else {
            return;
        };
        (runtime.sink)(RuntimeEvent::Log(copy_callback_string(message)));
    }));
}

unsafe extern "C" fn set_error_callback(user_data: *mut c_void, message: *const c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = runtime_from_ptr(user_data) else {
            return;
        };
        if runtime.last_error.is_none() {
            runtime.last_error = Some(copy_callback_string(message));
        }
    }));
}

unsafe extern "C" fn is_cancelled_callback(user_data: *mut c_void) -> u8 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime_from_ptr(user_data).is_none_or(|runtime| runtime.cancelled.load(Ordering::Relaxed))
            as u8
    }))
    .unwrap_or(1)
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn uds_request_callback(
    user_data: *mut c_void,
    request_id: u32,
    request: *const u8,
    request_len: usize,
    await_response: u8,
    response: *mut u8,
    response_capacity: usize,
    response_len: *mut usize,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = runtime_from_ptr(user_data) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if request.is_null() || request_len == 0 || response_len.is_null() {
            return runtime.fail("UDS callback received invalid pointers");
        }
        if runtime.cancelled.load(Ordering::Relaxed) {
            return STATUS_CANCELLED;
        }
        // SAFETY: the plugin keeps the request buffer alive for this callback.
        let request = unsafe { std::slice::from_raw_parts(request, request_len) };
        runtime.transport.0.isotp.cmd_id = wire_id(request_id);
        let mut received = Vec::new();
        let state = if await_response == 0 {
            runtime.transport.send_request(request, None)
        } else {
            runtime.transport.send_request(request, Some(&mut received))
        };
        if state != MsgState::Success {
            let detail = runtime
                .transport
                .0
                .last_error()
                .map(ToString::to_string)
                .unwrap_or_else(|| state.name().to_string());
            return runtime.fail(format!("UDS transport failed: {detail}"));
        }
        // SAFETY: `response_len` is a required out pointer validated above.
        unsafe { *response_len = received.len() };
        if received.len() > response_capacity {
            return STATUS_BUFFER_TOO_SMALL;
        }
        if !received.is_empty() {
            if response.is_null() {
                return runtime.fail("UDS response buffer is null");
            }
            // SAFETY: capacity was checked and the buffers do not overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(received.as_ptr(), response, received.len());
            }
        }
        STATUS_OK
    }))
    .unwrap_or(STATUS_ERROR)
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn compute_key_callback(
    user_data: *mut c_void,
    security_level: u32,
    variant: *const u8,
    variant_len: usize,
    seed: *const u8,
    seed_len: usize,
    key: *mut u8,
    key_capacity: usize,
    key_len: *mut usize,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = runtime_from_ptr(user_data) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if seed.is_null() || seed_len == 0 || key_len.is_null() {
            return runtime.fail("Seed & Key callback received invalid pointers");
        }
        // SAFETY: the plugin owns these input buffers for this callback. A
        // null variant is accepted only when its length is zero.
        let (seed, variant) = unsafe {
            (
                std::slice::from_raw_parts(seed, seed_len),
                if variant_len == 0 {
                    &[]
                } else if variant.is_null() {
                    return runtime.fail("Seed & Key variant pointer is null");
                } else {
                    std::slice::from_raw_parts(variant, variant_len)
                },
            )
        };
        let generated = match runtime.seed_key.compute(security_level, variant, seed) {
            Ok(value) => value,
            Err(error) => return runtime.fail(error.to_string()),
        };
        // SAFETY: `key_len` is a required out pointer validated above.
        unsafe { *key_len = generated.len() };
        if generated.len() > key_capacity {
            return STATUS_BUFFER_TOO_SMALL;
        }
        if !generated.is_empty() {
            if key.is_null() {
                return runtime.fail("Seed & Key output buffer is null");
            }
            // SAFETY: capacity was checked and buffers do not overlap.
            unsafe { std::ptr::copy_nonoverlapping(generated.as_ptr(), key, generated.len()) };
        }
        STATUS_OK
    }))
    .unwrap_or(STATUS_ERROR)
}

unsafe extern "C" fn load_image_callback(
    user_data: *mut c_void,
    path: *const c_char,
    base_address: u32,
    component: u32,
    image_handle: *mut u64,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = runtime_from_ptr(user_data) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if path.is_null() || image_handle.is_null() {
            return runtime.fail("image callback received invalid pointers");
        }
        let path = Path::new(&copy_callback_string(path)).to_path_buf();
        let image = match runtime.loader.load_image(&path, base_address, component) {
            Ok(image) => image,
            Err(error) => return runtime.fail(error.to_string()),
        };
        let handle = runtime.next_image_handle;
        runtime.next_image_handle = runtime.next_image_handle.wrapping_add(1).max(1);
        runtime.images.insert(handle, image);
        // SAFETY: the output pointer was validated above.
        unsafe { *image_handle = handle };
        STATUS_OK
    }))
    .unwrap_or(STATUS_ERROR)
}

unsafe extern "C" fn image_segment_count_callback(
    user_data: *mut c_void,
    image_handle: u64,
    count: *mut usize,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = runtime_from_ptr(user_data) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if count.is_null() {
            return runtime.fail("segment-count output pointer is null");
        }
        let Some(image) = runtime.images.get(&image_handle) else {
            return runtime.fail(format!("unknown image handle {image_handle}"));
        };
        // SAFETY: the output pointer was validated above.
        unsafe { *count = image.segments.len() };
        STATUS_OK
    }))
    .unwrap_or(STATUS_ERROR)
}

unsafe extern "C" fn image_segment_callback(
    user_data: *mut c_void,
    image_handle: u64,
    index: usize,
    address: *mut u32,
    data: *mut *const u8,
    data_len: *mut usize,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = runtime_from_ptr(user_data) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if address.is_null() || data.is_null() || data_len.is_null() {
            return runtime.fail("segment callback received invalid output pointers");
        }
        let Some(segment) = runtime
            .images
            .get(&image_handle)
            .and_then(|image| image.segments.get(index))
        else {
            return runtime.fail(format!(
                "image handle {image_handle} has no segment {index}"
            ));
        };
        // SAFETY: all out pointers were validated; the data allocation stays
        // alive until the plugin calls `free_image`.
        unsafe {
            *address = segment.address;
            *data = segment.data.as_ptr();
            *data_len = segment.data.len();
        }
        STATUS_OK
    }))
    .unwrap_or(STATUS_ERROR)
}

unsafe extern "C" fn free_image_callback(user_data: *mut c_void, image_handle: u64) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(runtime) = runtime_from_ptr(user_data) {
            runtime.images.remove(&image_handle);
        }
    }));
}

fn runtime_from_ptr<'a>(user_data: *mut c_void) -> Option<&'a mut PluginRuntime> {
    if user_data.is_null() {
        None
    } else {
        // SAFETY: every host callback receives the live `PluginRuntime`
        // pointer installed in `FlowContextV1` for the synchronous execution.
        Some(unsafe { &mut *user_data.cast::<PluginRuntime>() })
    }
}

fn copy_callback_string(message: *const c_char) -> String {
    if message.is_null() {
        return "<null>".to_string();
    }
    // SAFETY: plugin callback strings are NUL-terminated and valid for the
    // duration of the callback.
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

fn path_to_c_string(path: &Path) -> Result<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| Error::Plugin(format!("path contains a NUL byte: {}", path.display())))
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::Mutex;

    use crate::device::{close_device, open_device, scan_devices, AdapterKind};

    use super::*;

    #[test]
    #[ignore = "requires the three example plugin DLLs to be built first"]
    fn configured_plugins_complete_a_virtual_flash() {
        let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("Conf")
            .join("ExampleCar")
            .join("ExampleCar.toml");
        let config = VehicleFlashConfig::load(&config_path).unwrap();
        let choice = scan_devices(AdapterKind::Demo).unwrap().remove(0);
        let device = open_device(&choice, &config.can, config.can.response_id).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let sink: EventSink = Arc::new(move |event| captured.lock().unwrap().push(event));

        let (device, result) = execute_flow(
            device,
            &config,
            &config.source_dir.join("SampleApplication.hex"),
            Arc::new(AtomicBool::new(false)),
            sink,
        );
        close_device(device);
        if let Err(error) = result {
            let messages = events
                .lock()
                .unwrap()
                .iter()
                .map(|event| match event {
                    RuntimeEvent::Progress { percent, message } => {
                        format!("{percent}% {message}")
                    }
                    RuntimeEvent::Log(message) => message.clone(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            panic!("{error}\n{messages}");
        }

        let events = events.lock().unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Progress { percent: 100, .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Log(message) if message.contains("Loaded flow")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Log(message) if message.contains("Loaded file loader")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Log(message) if message.contains("Loaded Seed & Key")
        )));
    }
}
