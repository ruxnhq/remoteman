// remoteman patch (VENDORED.md, patch 9): WinUI's `InputDesktopResourceCursor`, which the
// generated bindings don't have, for `InputSystemCursorShape::Hidden`: a cursor made from a
// cursor resource in the app's own exe. The interface's IID and its methods' order are from the
// Windows App SDK runtime's Microsoft.UI.winmd (2.6.4): Create(UInt32), then CreateFromModule(String,
// UInt32). Create looks somewhere other than the exe (measured: "the specified resource name cannot
// be found" for a cursor the exe has), so the exe is named.

use super::bindings::InputCursor;
use windows_core::Interface;

windows_core::imp::define_winrt_interface!(
    IInputDesktopResourceCursorStatics,
    IInputDesktopResourceCursorStatics_Vtbl,
    0xf440dc37_a0b6_56eb_bcec_b024f2233d47
);

#[repr(C)]
#[allow(non_snake_case)]
pub struct IInputDesktopResourceCursorStatics_Vtbl {
    pub base__: windows_core::IInspectable_Vtbl,
    _Create: usize,
    pub CreateFromModule: unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void, u32, *mut *mut core::ffi::c_void) -> windows_core::HRESULT,
}

struct InputDesktopResourceCursor;

impl windows_core::RuntimeName for InputDesktopResourceCursor {
    const NAME: &'static str = "Microsoft.UI.Input.InputDesktopResourceCursor";
}

/// The cursor resource numbered `id` in the running exe, as a WinUI cursor.
pub(crate) fn from_resource(id: u32) -> windows_core::Result<InputCursor> {
    let statics: IInputDesktopResourceCursorStatics = windows_core::factory::<InputDesktopResourceCursor, _>()?;
    let exe = std::env::current_exe().map_err(|_| windows_core::Error::from_hresult(windows_core::HRESULT(0x80004005u32 as i32)))?;
    let name = windows_core::HSTRING::from(exe.as_os_str());
    let mut made = core::ptr::null_mut();
    // SAFETY: the factory's CreateFromModule, as the metadata declares it (an HSTRING passed
    // borrowed); the result is owned once returned.
    unsafe {
        (statics.vtable().CreateFromModule)(statics.as_raw(), core::mem::transmute_copy(&name), id, &mut made).ok()?;
        windows_core::IInspectable::from_raw(made).cast()
    }
}