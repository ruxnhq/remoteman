//! On Windows: each program's icon and version information, and the Windows App Runtime staged
//! beside the exes (`windows-reactor-setup` copies it into the profile's output folder,
//! `target\release`, and embeds the manifest that finds it beside the exe in both), which
//! scripts/win-host-setup.ps1 and scripts/win-release.ps1 then pack into the installers.
//! Elsewhere the crate is a stub and this does nothing.
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
    // Each program its own names (the UAC prompt names the host's by its description); the
    // version is the workspace's.
    for (bin, product) in [("remoteman-host-setup", "remoteman host"), ("remoteman-viewer-setup", "remoteman")] {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon.to_str().expect("icon path"));
        res.set("ProductName", product);
        res.set("FileDescription", &format!("{product} setup"));
        res.set("CompanyName", "remoteman");
        res.set("OriginalFilename", &format!("{bin}.exe"));
        res.set("InternalName", bin);
        res.compile_for(bin).expect("embed the installer's icon");
    }
    // The first build on a PC downloads the runtime from NuGet into %LOCALAPPDATA%\
    // windows-reactor-setup; later builds use that copy.
    windows_reactor_setup::as_self_contained();
}
