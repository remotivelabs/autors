//! Deliberately non-production Seed & Key implementation.
//!
//! This plugin exists only to exercise runtime loading and the `GenerateKeyEx`
//! boundary. It contains no production key material.

const RESULT_OK: i32 = 0;
const BUFFER_TOO_SMALL: i32 = 1;
const SECURITY_LEVEL_INVALID: i32 = 2;
const UNSPECIFIED_ERROR: i32 = 4;
const EXPECTED_LEVEL: u32 = 0x11;
const EXPECTED_SEED_LENGTH: usize = 16;
const KEY_LENGTH: usize = 32;

/// Generates the public example security record.
///
/// # Safety
///
/// `seed` must reference `seed_len` readable bytes. `key` must reference
/// `max_key_len` writable bytes, and `actual_len` must be a writable `u32`.
/// The buffers must remain valid and non-overlapping for the duration of the
/// call.
#[no_mangle]
pub unsafe extern "C" fn GenerateKeyEx(
    seed: *const u8,
    seed_len: u32,
    security_level: u32,
    _variant: *const u8,
    key: *mut u8,
    max_key_len: u32,
    actual_len: *mut u32,
) -> i32 {
    std::panic::catch_unwind(|| {
        if actual_len.is_null() {
            return UNSPECIFIED_ERROR;
        }
        // SAFETY: `actual_len` is a required writable out parameter.
        unsafe { *actual_len = KEY_LENGTH as u32 };
        if security_level != EXPECTED_LEVEL {
            return SECURITY_LEVEL_INVALID;
        }
        if seed.is_null() || seed_len as usize != EXPECTED_SEED_LENGTH {
            return UNSPECIFIED_ERROR;
        }
        if key.is_null() || max_key_len < KEY_LENGTH as u32 {
            return BUFFER_TOO_SMALL;
        }
        // SAFETY: lengths and pointers were validated above. The host owns
        // both non-overlapping buffers for the duration of this call.
        let (seed, output) = unsafe {
            (
                std::slice::from_raw_parts(seed, EXPECTED_SEED_LENGTH),
                std::slice::from_raw_parts_mut(key, KEY_LENGTH),
            )
        };
        for index in 0..EXPECTED_SEED_LENGTH {
            output[index] = seed[index].rotate_left((index % 7) as u32) ^ 0x5A;
            output[index + EXPECTED_SEED_LENGTH] =
                seed[EXPECTED_SEED_LENGTH - 1 - index].wrapping_add(index as u8);
        }
        RESULT_OK
    })
    .unwrap_or(UNSPECIFIED_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_provider_returns_a_32_byte_record() {
        let seed: Vec<u8> = (0..16).collect();
        let mut output = [0u8; KEY_LENGTH];
        let mut actual = 0u32;
        // SAFETY: all pointers refer to live buffers with the declared sizes.
        let status = unsafe {
            GenerateKeyEx(
                seed.as_ptr(),
                seed.len() as u32,
                EXPECTED_LEVEL,
                std::ptr::null(),
                output.as_mut_ptr(),
                output.len() as u32,
                &mut actual,
            )
        };
        assert_eq!(status, RESULT_OK);
        assert_eq!(actual, KEY_LENGTH as u32);
        assert_ne!(&output[..16], seed.as_slice());
    }
}
