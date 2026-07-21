//! Regenerate the C header from the exported ABI on every build.
//!
//! The Swift app imports `include/ac_ffi.h`; generating it here from
//! `src/lib.rs` means the header cannot drift from the Rust. A cbindgen failure
//! is a warning, not a hard error, so a header problem never blocks `cargo build`
//! (the committed header from the last good run stays in place).

fn main() {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let include = std::path::Path::new(&crate_dir).join("include");
    if let Err(e) = std::fs::create_dir_all(&include) {
        println!("cargo:warning=could not create {}: {e}", include.display());
        return;
    }
    let header = include.join("ac_ffi.h");

    match cbindgen::generate(&crate_dir) {
        Ok(bindings) => {
            bindings.write_to_file(&header);
        }
        Err(e) => {
            println!("cargo:warning=cbindgen could not generate {}: {e}", header.display());
        }
    }
}
