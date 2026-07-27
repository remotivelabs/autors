//! C smoke test: build.rs compiles examples/c_demo.c into a static library
//! with cc and links it into this test binary; here we call its entry point
//! directly to verify that "the C example compiles, links, and runs".
//! Also contains a freshness assertion that include/autors.h matches the
//! current cbindgen output.

use std::ffi::{c_char, CStr};
use std::path::Path;

// Reference this crate: ensures rustc carries libautors_ffi.rlib together
// with the `-l static=autors_c_demo` link flag emitted by build.rs into the
// test binary (an unused --extern would be dropped by lazy linking, taking
// its native link flags with it).
use autors_ffi as _;

extern "C" {
    /// Demo entry point from examples/c_demo.c: returns 0 on success, or the
    /// failing step number with details written into buf.
    fn autors_c_demo_run(buf: *mut c_char, buf_len: usize) -> i32;
}

#[test]
fn c_demo_compiles_links_and_runs() {
    let mut buf = vec![0i8; 8192];
    // SAFETY: buf is an exclusively owned writable buffer whose length
    // matches the value passed in; the C side guarantees NUL termination.
    let rc = unsafe { autors_c_demo_run(buf.as_mut_ptr(), buf.len()) };
    // SAFETY: buf is zero-initialized, and the C side's snprintf truncation
    // also preserves NUL termination.
    let out = unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    assert_eq!(rc, 0, "c demo failed at step {rc}:\n{out}");
    // Key output assertions: parse counts, address, conversion
    // (phys = raw*2 + 3), address update, write-out
    assert!(out.contains("modules=1 measurements=1"), "{out}");
    assert!(
        out.contains("measurement=EngineSpeed address=0x1000"),
        "{out}"
    );
    assert!(out.contains("type=UWORD"), "{out}");
    assert!(out.contains("conversion=Conv_EngineSpeed"), "{out}");
    assert!(out.contains("record_layout=RL_DEFAULT"), "{out}");
    assert!(out.contains("to_physical(10)=23"), "{out}");
    assert!(out.contains("to_raw(23)=10"), "{out}");
    assert!(out.contains("address_after_set=0x2000"), "{out}");
    assert!(out.contains("write_string="), "{out}");
}

#[test]
fn generated_header_is_up_to_date() {
    let crate_dir = env!("CARGO_MANIFEST_DIR");
    let config = cbindgen::Config::from_file(Path::new(crate_dir).join("cbindgen.toml"))
        .expect("failed to load cbindgen.toml");
    let bindings = cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .expect("cbindgen failed to parse autors-ffi");
    let mut regenerated = Vec::new();
    bindings.write(&mut regenerated);
    let on_disk = std::fs::read(Path::new(crate_dir).join("include").join("autors.h"))
        .expect("read autors.h");
    assert_eq!(
        regenerated, on_disk,
        "include/autors.h is stale — rebuild the crate (`cargo build -p autors-ffi`) to regenerate"
    );
}
