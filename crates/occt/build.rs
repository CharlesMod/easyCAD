//! Builds OpenCascade 7.8 statically (via `occt-sys`, cached under `target/OCCT`)
//! and compiles our C++ shim against it.

/// Static libs in dependency order (dependents first) for single-pass linkers.
const OCCT_LIBS: &[&str] = &[
    "TKDESTEP", "TKDESTL", "TKDE", "TKXCAF", "TKVCAF", "TKCAF", "TKV3d", "TKService", "TKLCAF", "TKCDF", "TKXSBase",
    "TKHLR", "TKFeat", "TKOffset", "TKFillet", "TKBool", "TKBO", "TKMesh",
    "TKShHealing", "TKPrim", "TKTopAlgo", "TKGeomAlgo", "TKBRep", "TKGeomBase",
    "TKG3d", "TKG2d", "TKMath", "TKernel",
];

fn main() {
    occt_sys::build_occt();
    let root = occt_sys::occt_path();
    let lib_dir = root.join("lib");
    let inc_dir = root.join("include");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    cxx_build::bridge("src/ffi.rs")
        .file("cpp/shim.cpp")
        .include(&inc_dir)
        .include("cpp")
        .std("c++17")
        .define("_USE_MATH_DEFINES", None)
        // OCC_CATCH_SIGNALS turns kernel segfaults into exceptions (see shim.h)
        .define("OCC_CONVERT_SIGNALS", None)
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-deprecated-declarations")
        .flag_if_supported("-Wno-unused-function")
        .compile("easycad-occt-shim");

    for lib in OCCT_LIBS {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    if std::env::var("TARGET").unwrap().contains("windows") {
        println!("cargo:rustc-link-lib=dylib=user32");
    }
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=cpp/shim.cpp");
    println!("cargo:rerun-if-changed=cpp/shim.h");
}
