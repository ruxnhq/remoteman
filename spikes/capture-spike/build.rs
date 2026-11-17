// Compiles the NVENC/CUDA C wrapper and the conversion shader.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=shim/nvenc_shim.c");
    println!("cargo:rerun-if-changed=shaders/convert.comp");
    cc::Build::new().file("shim/nvenc_shim.c").include("../../vendor/nv-codec-headers").warnings(false).compile("rm_nvenc_shim");
    println!("cargo:rustc-link-lib=dl");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("convert.spv");
    let ok = Command::new("glslc").args(["shaders/convert.comp", "-O", "-o"]).arg(&out).status().map(|s| s.success()).unwrap_or(false);
    assert!(ok, "glslc failed (is shaderc installed?)");
}
