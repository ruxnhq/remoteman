// Compiles the C helper that draws NVDEC pictures through OpenGL.
fn main() {
    println!("cargo:rerun-if-changed=shim/glview.c");
    cc::Build::new().file("shim/glview.c").include("../../vendor/nv-codec-headers").warnings(false).compile("rm_glview");
    println!("cargo:rustc-link-lib=epoxy");
    println!("cargo:rustc-link-lib=dl");
}
