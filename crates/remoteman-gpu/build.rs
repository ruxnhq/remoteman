// Compiles the conversion shader to SPIR-V (needs glslc, from shaderc).
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=shaders/convert.comp");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("convert.spv");
    let ok = Command::new("glslc").args(["shaders/convert.comp", "-O", "-o"]).arg(&out).status().map(|s| s.success()).unwrap_or(false);
    assert!(ok, "glslc failed (is shaderc installed?)");
}
