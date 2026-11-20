//! On Windows: the exe's icon and version information, and the Windows App Runtime staged beside
//! the exe (`windows-reactor-setup` copies it into the profile's output folder, `target\release`,
//! and embeds the manifest that finds it beside the exe), which scripts/win-host-setup.ps1 then
//! packs into the installer. Elsewhere the crate is a stub and this does nothing.
//!
//! The build dependencies are Windows-only, so the code that uses them is compiled only when the
//! build itself runs on Windows (a build script is compiled for the machine that builds).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/setup.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
        return;
    }
    #[cfg(windows)]
    windows_only();
}

#[cfg(windows)]
fn windows_only() {
    let icon = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/setup.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("icon path"));
    // The UAC prompt names the program by its description.
    res.set("ProductName", "remoteman host");
    res.set("FileDescription", "remoteman host setup");
    res.set("CompanyName", "remoteman");
    res.set("OriginalFilename", "remoteman-host-setup.exe");
    res.set("InternalName", "remoteman-host-setup");
    res.compile().expect("embed the installer's icon");
    // The first build on a PC downloads the runtime from NuGet into %LOCALAPPDATA%\
    // windows-reactor-setup; later builds use that copy.
    windows_reactor_setup::as_self_contained();
}
