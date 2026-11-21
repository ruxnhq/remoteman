//! On Windows: the exe's version information, and the Windows App Runtime staged beside the exe
//! (`windows-reactor-setup`: the runtime's DLLs and resources are copied into the profile's
//! output folder, `target\release`, and a manifest that finds them there is embedded), so the
//! app needs nothing installed. With `REMOTEMAN_FRAMEWORK_DEPENDENT=1` (the Store's MSIX) the
//! runtime is left out: the package depends on its framework package instead. Elsewhere the
//! crate is a stub and this does nothing.
//!
//! The build dependencies are Windows-only, so the code that uses them is compiled only when the
//! build itself runs on Windows (a build script is compiled for the machine that builds).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=REMOTEMAN_FRAMEWORK_DEPENDENT");
    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
        return;
    }
    #[cfg(windows)]
    windows_only();
}

#[cfg(windows)]
fn windows_only() {
    let mut res = winresource::WindowsResource::new();
    // The exe's icon (its first, which the taskbar, Start, Alt+Tab and the full-screen window
    // use): the viewer's app icon, made by assets/make-icon.ps1.
    println!("cargo:rerun-if-changed=assets/viewer.ico");
    let icon = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/viewer.ico");
    res.set_icon(icon.to_str().expect("icon path"));
    // Cursor resource 1: blank (32x32, nothing drawn), what Reactor's `InputSystemCursorShape::Hidden`
    // wears; the picture wears it while the pointer is captured (the computer's is drawn instead).
    println!("cargo:rerun-if-changed=assets/blank.cur");
    let blank = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/blank.cur");
    res.append_rc_content(&format!("1 CURSOR \"{}\"", blank.to_str().expect("cursor path").replace('\\', "\\\\")));
    res.set("ProductName", "remoteman");
    res.set("FileDescription", "remoteman");
    res.set("OriginalFilename", "remoteman.exe");
    res.set("InternalName", "remoteman");
    res.compile().expect("embed the app's version information");
    if std::env::var("REMOTEMAN_FRAMEWORK_DEPENDENT").as_deref() != Ok("1") {
        // The first build on a PC downloads the runtime from NuGet into %LOCALAPPDATA%\
        // windows-reactor-setup; later builds use that copy.
        windows_reactor_setup::as_self_contained();
    }
}
