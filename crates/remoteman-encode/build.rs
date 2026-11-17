// Compiles the C wrapper over NVENC and CUDA.
fn main() {
    println!("cargo:rerun-if-changed=shim/nvenc_shim.c");
    cc::Build::new().file("shim/nvenc_shim.c").include("../../vendor/nv-codec-headers").warnings(false).compile("rm_nvenc_shim");
    println!("cargo:rustc-link-lib=dl");
}
