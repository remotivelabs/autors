//! build.rs: cbindgen generates include/autors.h; on native builds it also
//! compiles examples/c_demo.c into a static library with cc (linked and
//! called by tests/c_smoke.rs).
//! The C compilation is skipped when cross-compiling (HOST != TARGET, e.g.
//! `cargo check --target x86_64-unknown-linux-gnu`) — cross environments
//! usually lack the matching C toolchain, and the C smoke test only runs
//! natively.

use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=examples/c_demo.c");

    // Generate the C header into the crate's include/ directory (kept under
    // version control; tests/c_smoke.rs asserts the header matches the
    // cbindgen output).
    // Note: Builder::with_crate does not load cbindgen.toml automatically;
    // with_config must be passed explicitly.
    let header = PathBuf::from(&crate_dir).join("include").join("autors.h");
    let config = cbindgen::Config::from_file(PathBuf::from(&crate_dir).join("cbindgen.toml"))
        .expect("failed to load cbindgen.toml");
    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
        .expect("cbindgen failed to parse autors-ffi")
        .write_to_file(&header);

    // Compile the C example only on native builds (linked into the test
    // binary).
    if env::var("HOST").expect("HOST not set") == env::var("TARGET").expect("TARGET not set") {
        cc::Build::new()
            .file("examples/c_demo.c")
            .include("include")
            .compile("autors_c_demo");
    }
}
