// Compiles the C wrapper over NVENC (and, on Linux, CUDA), and on Linux the one over VA-API
// (with FFmpeg's VA-API encoders).
fn main() {
    println!("cargo:rerun-if-changed=shim/nvenc_shim.c");
    cc::Build::new().file("shim/nvenc_shim.c").include("../../vendor/nv-codec-headers").warnings(false).compile("rm_nvenc_shim");
    // The target's system, not the one this script runs on: the Windows host is cross-built.
    let target = std::env::var("CARGO_CFG_TARGET_OS");
    if target.as_deref() != Ok("windows") {
        println!("cargo:rustc-link-lib=dl");
    }
    if target.as_deref() == Ok("linux") {
        println!("cargo:rerun-if-changed=shim/vaapi_shim.c");
        cc::Build::new().file("shim/vaapi_shim.c").warnings(true).compile("rm_vaapi_shim");
        for lib in ["avcodec", "avutil", "va", "va-drm"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
