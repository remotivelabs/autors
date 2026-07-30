use std::ffi::{c_char, CStr, CString};
use std::time::Duration;

use autors_security_flasher_sdk::{
    flow_struct_size, FlowContextV1, FlowPluginV1, ABI_VERSION_V1, LOADER_MODE_PACKAGE,
    LOADER_MODE_SEPARATE, LOAD_COMPONENT_PACKAGE_APPLICATION, LOAD_COMPONENT_PACKAGE_DRIVER,
    LOAD_COMPONENT_SINGLE, STATUS_CANCELLED, STATUS_ERROR, STATUS_OK,
};
use chrono::Datelike;
use sha2::{Digest, Sha256};

const SECURITY_SEED_LEVEL: u8 = 0x11;
const SECURITY_KEY_LEVEL: u8 = 0x12;
const PRECONDITION_ROUTINE_ID: u16 = 0x0203;
const SIGNATURE_ROUTINE_ID: u16 = 0x0202;
const ERASE_ROUTINE_ID: u16 = 0xFF00;
const DEPENDENCY_ROUTINE_ID: u16 = 0xFF01;
const FINGERPRINT_DID: u16 = 0xF184;
const BOOT_VERSION_DID: u16 = 0xF183;
const APP_VERSION_DID: u16 = 0xF189;
const ROUTINE_COMPLETED_STATUS: u8 = 0x04;
const ADDRESS_AND_LENGTH_FORMAT: u8 = 0x44;
const COMMUNICATION_TYPE: u8 = 0x01;
const APP_START: u32 = 0x0000_C000;
const APP_END: u32 = 0x0001_FFF7;
const APP_VALIDITY_END: u32 = 0x0001_FFFF;
const APP_ERASE_LENGTH: u32 = 0x0001_4000;
const DRIVER_START: u32 = 0x2000_0400;
const DRIVER_LENGTH: usize = 0x400;
const SRAM_START: u32 = 0x2000_0000;
const SRAM_END: u32 = 0x2000_4000;
const TESTER_SERIAL: &[u8; 6] = b"AUTORS";
const RESPONSE_CAPACITY: usize = 65_536;
const KEY_CAPACITY: usize = 512;

static PLUGIN_NAME: &[u8] = b"ExampleCar security flow\0";

static API: FlowPluginV1 = FlowPluginV1 {
    abi_version: ABI_VERSION_V1,
    struct_size: std::mem::size_of::<FlowPluginV1>(),
    plugin_name,
    execute,
};

unsafe extern "C" fn plugin_name() -> *const c_char {
    PLUGIN_NAME.as_ptr().cast::<c_char>()
}

unsafe extern "C" fn execute(context: *const FlowContextV1) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the host supplies a live context for this synchronous call.
        let context = unsafe { Context::new(context) }?;
        run(&context)
    }));
    match result {
        Ok(Ok(())) => STATUS_OK,
        Ok(Err(error)) => {
            if let Ok(context) = unsafe { Context::new(context) } {
                context.set_error(&error.message);
            }
            if error.cancelled {
                STATUS_CANCELLED
            } else {
                STATUS_ERROR
            }
        }
        Err(_) => {
            if let Ok(context) = unsafe { Context::new(context) } {
                context.set_error("flow plugin panicked");
            }
            STATUS_ERROR
        }
    }
}

fn run(context: &Context<'_>) -> FlowResult<()> {
    context.report(0, "Loading images through the configured file-loader DLL")?;
    let driver_address = if context.raw.flash_driver_address == 0 {
        DRIVER_START
    } else {
        context.raw.flash_driver_address
    };
    let (raw_application, raw_driver) = match context.raw.loader_mode {
        LOADER_MODE_SEPARATE => (
            context.load_image(
                context.application_path()?,
                APP_START,
                LOAD_COMPONENT_SINGLE,
            )?,
            context.load_image(
                context.flash_driver_path()?,
                driver_address,
                LOAD_COMPONENT_SINGLE,
            )?,
        ),
        LOADER_MODE_PACKAGE => (
            context.load_image(
                context.application_path()?,
                APP_START,
                LOAD_COMPONENT_PACKAGE_APPLICATION,
            )?,
            context.load_image(
                context.application_path()?,
                driver_address,
                LOAD_COMPONENT_PACKAGE_DRIVER,
            )?,
        ),
        value => return Err(FlowError::new(format!("unsupported loader mode {value}"))),
    };
    let application = normalize_application(raw_application)?;
    let driver = validate_driver(&raw_driver, driver_address)?;
    context.log(&format!(
        "Prepared application ({} bytes) and flash driver ({} bytes)",
        application.data.len(),
        driver.data.len()
    ));

    context.report(3, "Functional request: return to default session")?;
    context.request(context.raw.functional_request_id, &[0x10, 0x81], false)?;
    std::thread::sleep(Duration::from_millis(50));

    context.report(4, "Physical request: enter extended session")?;
    positive(
        0x10,
        &context.request(context.raw.physical_request_id, &[0x10, 0x03], true)?,
        "extended session",
    )?;

    context.report(6, "Checking programming preconditions")?;
    routine(
        context,
        PRECONDITION_ROUTINE_ID,
        &[],
        false,
        "precondition check",
    )?;

    context.report(8, "Disabling diagnostic trouble-code updates")?;
    positive(
        0x85,
        &context.request(context.raw.functional_request_id, &[0x85, 0x02], true)?,
        "disable DTC updates",
    )?;

    context.report(9, "Disabling normal network communication")?;
    positive(
        0x28,
        &context.request(
            context.raw.functional_request_id,
            &[0x28, 0x03, COMMUNICATION_TYPE],
            true,
        )?,
        "disable communication",
    )?;

    read_version(context, BOOT_VERSION_DID, "Boot software", 10);

    context.report(12, "Entering programming session")?;
    positive(
        0x10,
        &context.request(context.raw.physical_request_id, &[0x10, 0x02], true)?,
        "programming session",
    )?;

    unlock(context)?;
    write_fingerprint(context)?;

    download_and_verify(context, &driver, 20, 34, "Flash driver")?;

    context.report(38, "Erasing the complete application region")?;
    let mut erase_record = vec![ADDRESS_AND_LENGTH_FORMAT];
    erase_record.extend_from_slice(&APP_START.to_be_bytes());
    erase_record.extend_from_slice(&APP_ERASE_LENGTH.to_be_bytes());
    routine(
        context,
        ERASE_ROUTINE_ID,
        &erase_record,
        true,
        "application erase",
    )?;

    download_and_verify(context, &application, 40, 84, "Application")?;

    context.report(88, "Checking programming dependencies")?;
    routine(
        context,
        DEPENDENCY_ROUTINE_ID,
        &[],
        true,
        "programming dependency check",
    )?;

    context.report(94, "Resetting ECU")?;
    positive(
        0x11,
        &context.request(context.raw.physical_request_id, &[0x11, 0x01], true)?,
        "ECU reset",
    )?;
    std::thread::sleep(Duration::from_millis(2500));

    if let Err(error) = restore_bus(context) {
        if error.cancelled {
            return Err(error);
        }
        context.log(&format!(
            "Post-flash bus restoration did not complete: {}",
            error.message
        ));
    }

    context.report(100, "Security flashing completed")?;
    Ok(())
}

fn unlock(context: &Context<'_>) -> FlowResult<()> {
    context.report(14, "Requesting security seed (27 11)")?;
    let response = context.request(
        context.raw.physical_request_id,
        &[0x27, SECURITY_SEED_LEVEL],
        true,
    )?;
    positive(0x27, &response, "request security seed")?;
    if response.len() != 18 || response.get(1) != Some(&SECURITY_SEED_LEVEL) {
        return Err(FlowError::new(format!(
            "security seed response has {} bytes; expected 18",
            response.len()
        )));
    }
    let key = context.compute_key(u32::from(SECURITY_SEED_LEVEL), &response[2..])?;
    if key.len() != 32 {
        return Err(FlowError::new(format!(
            "Seed & Key DLL returned {} bytes; expected 32",
            key.len()
        )));
    }
    context.report(16, "Sending security record (27 12)")?;
    let mut request = vec![0x27, SECURITY_KEY_LEVEL];
    request.extend_from_slice(&key);
    let response = context.request(context.raw.physical_request_id, &request, true)?;
    positive(0x27, &response, "send security record")
}

fn write_fingerprint(context: &Context<'_>) -> FlowResult<()> {
    context.report(18, "Writing programming fingerprint")?;
    let now = chrono::Local::now();
    let mut request = vec![
        0x2E,
        (FINGERPRINT_DID >> 8) as u8,
        FINGERPRINT_DID as u8,
        to_bcd((now.year() % 100) as u8),
        to_bcd(now.month() as u8),
        to_bcd(now.day() as u8),
    ];
    request.extend_from_slice(TESTER_SERIAL);
    let response = context.request(context.raw.physical_request_id, &request, true)?;
    positive(0x2E, &response, "write programming fingerprint")
}

fn download_and_verify(
    context: &Context<'_>,
    image: &ContiguousImage,
    start_percent: i32,
    end_percent: i32,
    label: &str,
) -> FlowResult<()> {
    download(context, image, start_percent, end_percent - 2, label)?;
    context.report(
        end_percent,
        &format!("{label} secure signature check (RID 0x{SIGNATURE_ROUTINE_ID:04X})"),
    )?;
    let signature = example_signature(&image.data);
    routine(
        context,
        SIGNATURE_ROUTINE_ID,
        &signature,
        true,
        &format!("{label} signature check"),
    )
}

fn download(
    context: &Context<'_>,
    image: &ContiguousImage,
    start_percent: i32,
    end_percent: i32,
    label: &str,
) -> FlowResult<()> {
    context.report(
        start_percent,
        &format!(
            "Requesting {label} download at 0x{:08X} ({} bytes)",
            image.address,
            image.data.len()
        ),
    )?;
    let mut request = vec![0x34, 0x00, ADDRESS_AND_LENGTH_FORMAT];
    request.extend_from_slice(&image.address.to_be_bytes());
    request.extend_from_slice(&(image.data.len() as u32).to_be_bytes());
    let response = context.request(context.raw.physical_request_id, &request, true)?;
    positive(0x34, &response, &format!("request {label} download"))?;
    let max_block = parse_download_block_size(&response)?;
    let chunk_size = max_block
        .saturating_sub(2)
        .min(context.raw.transfer_block_size);
    if chunk_size == 0 {
        return Err(FlowError::new("negotiated transfer block size is zero"));
    }

    let mut sent = 0usize;
    let mut sequence = 1u8;
    while sent < image.data.len() {
        context.check_cancelled()?;
        let end = (sent + chunk_size).min(image.data.len());
        let mut request = vec![0x36, sequence];
        request.extend_from_slice(&image.data[sent..end]);
        let response = context.request(context.raw.physical_request_id, &request, true)?;
        positive(0x36, &response, &format!("transfer {label} block"))?;
        if response.get(1) != Some(&sequence) {
            return Err(FlowError::new(format!(
                "ECU did not echo transfer sequence {sequence}"
            )));
        }
        sent = end;
        sequence = sequence.wrapping_add(1);
        let percent = start_percent
            + ((end_percent - start_percent) as f64 * sent as f64 / image.data.len() as f64) as i32;
        context.report(percent, &format!("Downloading {label}: {sent} bytes"))?;
    }

    let response = context.request(context.raw.physical_request_id, &[0x37], true)?;
    positive(0x37, &response, &format!("finish {label} transfer"))
}

fn routine(
    context: &Context<'_>,
    routine_id: u16,
    record: &[u8],
    require_status: bool,
    label: &str,
) -> FlowResult<()> {
    let mut request = vec![0x31, 0x01, (routine_id >> 8) as u8, routine_id as u8];
    request.extend_from_slice(record);
    let response = context.request(context.raw.physical_request_id, &request, true)?;
    positive(0x31, &response, label)?;
    if response.len() < 4 || response[1] != 0x01 || response[2..4] != request[2..4] {
        return Err(FlowError::new(format!(
            "{label} returned an unexpected routine response"
        )));
    }
    if require_status {
        let status = response.get(4).copied().unwrap_or_default();
        if status != ROUTINE_COMPLETED_STATUS {
            return Err(FlowError::new(format!(
                "{label} returned status 0x{status:02X}; expected 0x{ROUTINE_COMPLETED_STATUS:02X}"
            )));
        }
    }
    Ok(())
}

fn read_version(context: &Context<'_>, did: u16, label: &str, percent: i32) {
    let request = [0x22, (did >> 8) as u8, did as u8];
    match context
        .request(context.raw.physical_request_id, &request, true)
        .and_then(|response| {
            positive(0x22, &response, label)?;
            Ok(response)
        }) {
        Ok(response) if response.len() >= 3 => context.report_without_cancel(
            percent,
            &format!("{label}: {}", String::from_utf8_lossy(&response[3..])),
        ),
        Ok(_) => context.log(&format!("{label} returned no version data")),
        Err(error) => context.log(&format!("{label} read was skipped: {}", error.message)),
    }
}

fn restore_bus(context: &Context<'_>) -> FlowResult<()> {
    context.report(97, "Restoring extended session")?;
    positive(
        0x10,
        &context.request(context.raw.functional_request_id, &[0x10, 0x03], true)?,
        "restore extended session",
    )?;
    read_version(context, APP_VERSION_DID, "Application software", 98);

    context.report(98, "Restoring normal network communication")?;
    positive(
        0x28,
        &context.request(
            context.raw.functional_request_id,
            &[0x28, 0x00, COMMUNICATION_TYPE],
            true,
        )?,
        "restore communication",
    )?;

    context.report(99, "Restoring diagnostic trouble-code updates")?;
    positive(
        0x85,
        &context.request(context.raw.functional_request_id, &[0x85, 0x01], true)?,
        "restore DTC updates",
    )?;

    context.request(context.raw.functional_request_id, &[0x10, 0x81], false)?;
    std::thread::sleep(Duration::from_millis(50));
    context.report(99, "Clearing flash-process DTCs")?;
    positive(
        0x14,
        &context.request(
            context.raw.functional_request_id,
            &[0x14, 0xFF, 0xFF, 0xFF],
            true,
        )?,
        "clear flash-process DTCs",
    )
}

fn positive(request_sid: u8, response: &[u8], label: &str) -> FlowResult<()> {
    if response.len() >= 3 && response[0] == 0x7F {
        return Err(FlowError::new(format!(
            "{label} failed with NRC 0x{:02X}",
            response[2]
        )));
    }
    if response.first() != Some(&request_sid.wrapping_add(0x40)) {
        return Err(FlowError::new(format!(
            "{label} returned an unexpected response"
        )));
    }
    Ok(())
}

fn parse_download_block_size(response: &[u8]) -> FlowResult<usize> {
    if response.len() < 3 {
        return Err(FlowError::new("RequestDownload response is too short"));
    }
    let length = usize::from(response[1] >> 4);
    if length == 0 || length > 8 || response.len() < 2 + length {
        return Err(FlowError::new(
            "RequestDownload response has an invalid length format",
        ));
    }
    let mut value = 0usize;
    for &byte in &response[2..2 + length] {
        value = value
            .checked_shl(8)
            .and_then(|value| value.checked_add(usize::from(byte)))
            .ok_or_else(|| FlowError::new("RequestDownload block length overflow"))?;
    }
    Ok(value)
}

fn normalize_application(image: Image) -> FlowResult<ContiguousImage> {
    let length = (APP_END - APP_START + 1) as usize;
    let mut data = vec![0xFF; length];
    let mut supplied = vec![false; length];
    for segment in image.segments {
        for (offset, value) in segment.data.into_iter().enumerate() {
            let address = u64::from(segment.address) + offset as u64;
            if (u64::from(APP_START)..=u64::from(APP_END)).contains(&address) {
                let target = (address - u64::from(APP_START)) as usize;
                if supplied[target] && data[target] != value {
                    return Err(FlowError::new(format!(
                        "conflicting application bytes at 0x{address:08X}"
                    )));
                }
                data[target] = value;
                supplied[target] = true;
            } else if (u64::from(APP_END + 1)..=u64::from(APP_VALIDITY_END)).contains(&address) {
                // The dependency routine owns the validity bytes.
            } else {
                return Err(FlowError::new(format!(
                    "application data at 0x{address:08X} is outside the allowed range"
                )));
            }
        }
    }
    if !supplied.iter().take(8).all(|value| *value) {
        return Err(FlowError::new("application vector table is incomplete"));
    }
    let stack = u32::from_le_bytes(data[0..4].try_into().unwrap_or_default());
    let reset = u32::from_le_bytes(data[4..8].try_into().unwrap_or_default());
    if !(SRAM_START..=SRAM_END).contains(&stack) {
        return Err(FlowError::new(format!(
            "initial stack pointer 0x{stack:08X} is outside the allowed SRAM range"
        )));
    }
    if reset & 1 == 0 || !(APP_START..=APP_END).contains(&(reset & !1)) {
        return Err(FlowError::new(format!(
            "reset handler 0x{reset:08X} is outside the application range"
        )));
    }
    Ok(ContiguousImage {
        address: APP_START,
        data,
    })
}

fn validate_driver(image: &Image, expected_address: u32) -> FlowResult<ContiguousImage> {
    let [segment] = image.segments.as_slice() else {
        return Err(FlowError::new(
            "flash driver must contain exactly one contiguous segment",
        ));
    };
    if segment.address != expected_address || segment.data.len() != DRIVER_LENGTH {
        return Err(FlowError::new(format!(
            "flash driver must start at 0x{expected_address:08X} and contain 0x{DRIVER_LENGTH:X} bytes"
        )));
    }
    Ok(ContiguousImage {
        address: segment.address,
        data: segment.data.clone(),
    })
}

fn example_signature(data: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(data);
    let mut signature = vec![0xFF; 64];
    signature[..16].copy_from_slice(&digest[..16]);
    signature
}

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

struct Context<'a> {
    raw: &'a FlowContextV1,
}

impl<'a> Context<'a> {
    unsafe fn new(pointer: *const FlowContextV1) -> FlowResult<Self> {
        if pointer.is_null() {
            return Err(FlowError::new("flow context is null"));
        }
        // SAFETY: the host guarantees a live context for the execute call.
        let raw = unsafe { &*pointer };
        if raw.abi_version != ABI_VERSION_V1
            || raw.struct_size < std::mem::size_of::<FlowContextV1>()
        {
            return Err(FlowError::new("flow context ABI is incompatible"));
        }
        Ok(Self { raw })
    }

    fn application_path(&self) -> FlowResult<&CStr> {
        self.path(self.raw.application_path, "application path")
    }

    fn flash_driver_path(&self) -> FlowResult<&CStr> {
        self.path(self.raw.flash_driver_path, "flash-driver path")
    }

    fn path(&self, pointer: *const c_char, label: &str) -> FlowResult<&CStr> {
        if pointer.is_null() {
            return Err(FlowError::new(format!("{label} is null")));
        }
        // SAFETY: host path strings are NUL-terminated and live throughout execute.
        Ok(unsafe { CStr::from_ptr(pointer) })
    }

    fn report(&self, percent: i32, message: &str) -> FlowResult<()> {
        self.check_cancelled()?;
        self.report_without_cancel(percent, message);
        Ok(())
    }

    fn report_without_cancel(&self, percent: i32, message: &str) {
        if let Ok(message) = c_string(message) {
            // SAFETY: the CString lives through the synchronous callback.
            unsafe { (self.raw.report)(self.raw.user_data, percent, message.as_ptr()) };
        }
    }

    fn log(&self, message: &str) {
        if let Ok(message) = c_string(message) {
            // SAFETY: the CString lives through the synchronous callback.
            unsafe { (self.raw.log)(self.raw.user_data, message.as_ptr()) };
        }
    }

    fn set_error(&self, message: &str) {
        if let Ok(message) = c_string(message) {
            // SAFETY: the CString lives through the synchronous callback.
            unsafe { (self.raw.set_error)(self.raw.user_data, message.as_ptr()) };
        }
    }

    fn check_cancelled(&self) -> FlowResult<()> {
        // SAFETY: the host callback is valid throughout execute.
        if unsafe { (self.raw.is_cancelled)(self.raw.user_data) } != 0 {
            Err(FlowError::cancelled())
        } else {
            Ok(())
        }
    }

    fn request(
        &self,
        request_id: u32,
        request: &[u8],
        await_response: bool,
    ) -> FlowResult<Vec<u8>> {
        self.check_cancelled()?;
        let mut response = vec![0u8; RESPONSE_CAPACITY];
        let mut response_len = 0usize;
        // SAFETY: all buffers and out pointers live through the callback.
        let status = unsafe {
            (self.raw.uds_request)(
                self.raw.user_data,
                request_id,
                request.as_ptr(),
                request.len(),
                await_response as u8,
                response.as_mut_ptr(),
                response.len(),
                &mut response_len,
            )
        };
        if status == STATUS_CANCELLED {
            return Err(FlowError::cancelled());
        }
        if status != STATUS_OK {
            return Err(FlowError::new(format!(
                "host UDS callback failed with status {status}"
            )));
        }
        if response_len > response.len() {
            return Err(FlowError::new(format!(
                "UDS response needs {response_len} bytes, exceeding the plugin buffer"
            )));
        }
        response.truncate(response_len);
        Ok(response)
    }

    fn compute_key(&self, level: u32, seed: &[u8]) -> FlowResult<Vec<u8>> {
        let mut key = vec![0u8; KEY_CAPACITY];
        let mut key_len = 0usize;
        // SAFETY: all buffers and out pointers live through the callback.
        let status = unsafe {
            (self.raw.compute_key)(
                self.raw.user_data,
                level,
                std::ptr::null(),
                0,
                seed.as_ptr(),
                seed.len(),
                key.as_mut_ptr(),
                key.len(),
                &mut key_len,
            )
        };
        if status != STATUS_OK || key_len > key.len() {
            return Err(FlowError::new(format!(
                "host Seed & Key callback failed with status {status}"
            )));
        }
        key.truncate(key_len);
        Ok(key)
    }

    fn load_image(&self, path: &CStr, base_address: u32, component: u32) -> FlowResult<Image> {
        let mut handle = 0u64;
        // SAFETY: the path and handle out pointer live through the callback.
        let status = unsafe {
            (self.raw.load_image)(
                self.raw.user_data,
                path.as_ptr(),
                base_address,
                component,
                &mut handle,
            )
        };
        if status != STATUS_OK {
            return Err(FlowError::new(format!(
                "host file-loader callback failed with status {status}"
            )));
        }
        let result = self.copy_image(handle);
        // SAFETY: the handle was returned by the host and is released once.
        unsafe { (self.raw.free_image)(self.raw.user_data, handle) };
        result
    }

    fn copy_image(&self, handle: u64) -> FlowResult<Image> {
        let mut count = 0usize;
        // SAFETY: `count` is a valid out pointer.
        let status =
            unsafe { (self.raw.image_segment_count)(self.raw.user_data, handle, &mut count) };
        if status != STATUS_OK || count == 0 {
            return Err(FlowError::new(format!(
                "host returned no image segments (status {status})"
            )));
        }
        let mut segments = Vec::with_capacity(count);
        for index in 0..count {
            let mut address = 0u32;
            let mut data = std::ptr::null();
            let mut data_len = 0usize;
            // SAFETY: all out pointers remain live through the callback.
            let status = unsafe {
                (self.raw.image_segment)(
                    self.raw.user_data,
                    handle,
                    index,
                    &mut address,
                    &mut data,
                    &mut data_len,
                )
            };
            if status != STATUS_OK || data.is_null() || data_len == 0 {
                return Err(FlowError::new(format!(
                    "host returned invalid image segment {index} (status {status})"
                )));
            }
            // SAFETY: the host keeps segment bytes alive until `free_image`.
            let bytes = unsafe { std::slice::from_raw_parts(data, data_len) };
            segments.push(Segment {
                address,
                data: bytes.to_vec(),
            });
        }
        Ok(Image { segments })
    }
}

#[derive(Debug)]
struct Image {
    segments: Vec<Segment>,
}

#[derive(Debug, Clone)]
struct Segment {
    address: u32,
    data: Vec<u8>,
}

struct ContiguousImage {
    address: u32,
    data: Vec<u8>,
}

#[derive(Debug)]
struct FlowError {
    message: String,
    cancelled: bool,
}

type FlowResult<T> = std::result::Result<T, FlowError>;

impl FlowError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cancelled: false,
        }
    }

    fn cancelled() -> Self {
        Self {
            message: "operation cancelled".to_string(),
            cancelled: true,
        }
    }
}

fn c_string(message: &str) -> Result<CString, std::ffi::NulError> {
    CString::new(message.replace('\0', "?"))
}

#[no_mangle]
pub extern "C" fn autors_security_flow_v1() -> *const FlowPluginV1 {
    debug_assert_eq!(API.struct_size, flow_struct_size());
    &API
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_image() -> Image {
        Image {
            segments: vec![Segment {
                address: APP_START,
                data: vec![0x00, 0x10, 0x00, 0x20, 0x01, 0xC1, 0x00, 0x00, 0x11, 0x22],
            }],
        }
    }

    #[test]
    fn application_normalization_fills_gaps() {
        let image = normalize_application(valid_image()).unwrap();
        assert_eq!(image.address, APP_START);
        assert_eq!(image.data.len(), 0x13FF8);
        assert_eq!(image.data[9], 0x22);
        assert_eq!(image.data[10], 0xFF);
    }

    #[test]
    fn public_signature_has_fixed_wire_length_and_padding() {
        let signature = example_signature(b"public example");
        assert_eq!(signature.len(), 64);
        assert!(signature[16..].iter().all(|value| *value == 0xFF));
    }

    #[test]
    fn download_response_block_size_is_big_endian() {
        assert_eq!(
            parse_download_block_size(&[0x74, 0x20, 0x04, 0x02]).unwrap(),
            1026
        );
    }
}
