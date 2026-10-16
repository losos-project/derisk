//! Compiles the vendored libfacedetection and generates its bindings.
//!
//! The library is three C++ files and needs nothing but a C++ compiler.
//! Its SIMD paths are picked here: NEON on aarch64, where every CPU has it,
//! and plain C++ on x86_64, since AVX2 is not on every PC the image boots
//! and a few small frames a second need no more.

use std::{env, path::PathBuf};

fn main() {
    let vendor = PathBuf::from("vendor");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++11")
        .include(&vendor)
        .files(
            [
                "facedetectcnn.cpp",
                "facedetectcnn-model.cpp",
                "facedetectcnn-data.cpp",
            ]
            .map(|f| vendor.join(f)),
        )
        // Upstream's own warnings, not ours to fix in a copy.
        .warnings(false)
        .opt_level(3);
    if arch == "aarch64" {
        build.define("_ENABLE_NEON", None);
    }
    build.compile("facedetection");

    // Only the entry point and its buffer sizes: the header's inline
    // classes are the library's internals.
    let bindings = bindgen::Builder::default()
        .header(vendor.join("facedetectcnn.h").to_string_lossy())
        .clang_args(["-x", "c++", "-std=c++11"])
        .clang_arg(format!("-I{}", vendor.display()))
        .allowlist_function("facedetect_cnn")
        .allowlist_var("FACEDETECTION_RESULT_.*")
        .generate()
        .expect("libfacedetection's header did not parse");
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("cannot write the bindings");
    println!("cargo::rerun-if-changed=vendor");
}
