//! On Windows: the exe's icon and version information, and the Windows App Runtime staged beside
//! the exe (`windows-reactor-setup`: the runtime's DLLs and resources are copied into the
//! profile's output folder, `target\release`, and a manifest that finds them there is embedded),
//! so the app needs nothing installed. Elsewhere the crate is a stub and this does nothing.
//!
//! The build dependencies are Windows-only, so the code that uses them is compiled only when the
//! build itself runs on Windows (a build script is compiled for the machine that builds).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/sharing.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
        return;
    }
    #[cfg(windows)]
    windows_only();
}

#[cfg(windows)]
fn windows_only() {
    let icon = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/sharing.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("icon path"));
    // Windows names the app by these, in its notifications among other places.
    res.set("ProductName", "Remoteman Host");
    res.set("FileDescription", "Remoteman Host");
    res.set("OriginalFilename", "remoteman-host-app.exe");
    res.set("InternalName", "remoteman-host-app");
    res.compile().expect("embed the app's icon");
    // The first build on a PC downloads the runtime from NuGet into %LOCALAPPDATA%\
    // windows-reactor-setup; later builds use that copy.
    windows_reactor_setup::as_self_contained();
}
