// Compiles the C wrapper over NVENC (and, on Linux, CUDA).
fn main() {
    println!("cargo:rerun-if-changed=shim/nvenc_shim.c");
    cc::Build::new().file("shim/nvenc_shim.c").include("../../vendor/nv-codec-headers").warnings(false).compile("rm_nvenc_shim");
    // The target's system, not the one this script runs on: the Windows host is cross-built.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        println!("cargo:rustc-link-lib=dl");
    }
}
