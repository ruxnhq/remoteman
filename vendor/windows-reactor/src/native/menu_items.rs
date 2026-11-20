//! remoteman patch (VENDORED.md, patch 6): menu items with a check mark (WinUI's
//! `ToggleMenuFlyoutItem`) or a radio button (`RadioMenuFlyoutItem`), and items dressed with a
//! glyph icon, a tooltip or the text of a keyboard shortcut. The bindings below are
//! windows-bindgen's output for those two classes over this commit's winmd
//! (`crates/tools/reactor-metadata/winmd`), with the filter
//! `ToggleMenuFlyoutItem::CreateInstance`, `IToggleMenuFlyoutItem::{get_IsChecked,
//! put_IsChecked}`, `RadioMenuFlyoutItem::CreateInstance`, `IRadioMenuFlyoutItem::{put_GroupName,
//! put_IsChecked}`; `IMenuFlyoutItem`'s `put_Icon` and `put_KeyboardAcceleratorTextOverride` were
//! added to `bindings.rs` from the same output.

use super::bindings::*;
use super::{HSTRING, IInspectable, Interface, Key, MenuItem};

/// The native item for a menu entry that is clicked (an item, a check item, a radio item, or
/// one of those dressed), and the key its click reports. `None` for a separator or a submenu,
/// which the caller builds.
pub(super) fn native_item(entry: &MenuItem) -> windows_core::Result<Option<(MenuFlyoutItem, Key)>> {
    let finish = |item: MenuFlyoutItem, label: &str, enabled: bool| -> windows_core::Result<MenuFlyoutItem> {
        item.SetText(label)?;
        item.cast::<IControl>()?.SetIsEnabled(enabled)?;
        Ok(item)
    };
    Ok(Some(match entry {
        MenuItem::Item { key, label, enabled } => (finish(MenuFlyoutItem::new()?, label, *enabled)?, key.clone()),
        MenuItem::Check { key, label, enabled, checked } => {
            let item = ToggleMenuFlyoutItem::new()?;
            item.SetIsChecked(*checked)?;
            (finish(item.cast()?, label, *enabled)?, key.clone())
        }
        MenuItem::Radio { key, label, enabled, checked, group } => {
            let item = RadioMenuFlyoutItem::new()?;
            item.SetGroupName(group)?;
            item.SetIsChecked(*checked)?;
            (finish(item.cast()?, label, *enabled)?, key.clone())
        }
        MenuItem::Dressed { item, glyph, tooltip, accelerator } => {
            let Some((native, key)) = native_item(item)? else { return Ok(None) };
            if let Some(glyph) = glyph {
                let icon = FontIcon::new()?;
                icon.SetGlyph(glyph)?;
                native.SetIcon(&icon.cast::<IconElement>()?)?;
            }
            if let Some(text) = accelerator {
                native.SetKeyboardAcceleratorTextOverride(text)?;
            }
            if let Some(tip) = tooltip {
                let text: IInspectable = windows_reference::IReference::from(HSTRING::from(&**tip)).into();
                ToolTipService::SetToolTip(&native.cast::<DependencyObject>()?, &text)?;
            }
            (native, key)
        }
        MenuItem::Separator { .. } | MenuItem::Submenu { .. } => return Ok(None),
    }))
}
windows_core::imp::define_winrt_interface!(
    IRadioMenuFlyoutItem,
    IRadioMenuFlyoutItem_Vtbl,
    0x9d6f45f5_5532_5669_8f02_05735953026a
);
impl IRadioMenuFlyoutItem {
    pub(crate) fn SetIsChecked(&self, value: bool) -> windows_core::Result<()> {
        unsafe {
            (windows_core::Interface::vtable(self).SetIsChecked)(
                windows_core::Interface::as_raw(self),
                value,
            )
            .ok()
        }
    }
    pub(crate) fn SetGroupName(&self, value: &str) -> windows_core::Result<()> {
        unsafe {
            (windows_core::Interface::vtable(self).SetGroupName)(
                windows_core::Interface::as_raw(self),
                core::mem::transmute_copy(&windows_core::HSTRING::from(value)),
            )
            .ok()
        }
    }
}
#[repr(C)]
pub struct IRadioMenuFlyoutItem_Vtbl {
    pub base__: windows_core::IInspectable_Vtbl,
    IsChecked: usize,
    pub SetIsChecked:
        unsafe extern "system" fn(*mut core::ffi::c_void, bool) -> windows_core::HRESULT,
    GroupName: usize,
    pub SetGroupName: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *mut core::ffi::c_void,
    ) -> windows_core::HRESULT,
}
windows_core::imp::define_winrt_interface!(
    IRadioMenuFlyoutItemFactory,
    IRadioMenuFlyoutItemFactory_Vtbl,
    0x113822d2_35a3_5b0d_9d0d_c3830ec1973b
);
#[repr(C)]
pub struct IRadioMenuFlyoutItemFactory_Vtbl {
    pub base__: windows_core::IInspectable_Vtbl,
    pub CreateInstance: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
    ) -> windows_core::HRESULT,
}
windows_core::imp::define_winrt_interface!(
    IToggleMenuFlyoutItem,
    IToggleMenuFlyoutItem_Vtbl,
    0x1803f260_67e4_5bc1_a63a_123510167bb8
);
impl IToggleMenuFlyoutItem {
    pub(crate) fn IsChecked(&self) -> windows_core::Result<bool> {
        unsafe {
            let mut result__ = core::mem::zeroed();
            (windows_core::Interface::vtable(self).IsChecked)(
                windows_core::Interface::as_raw(self),
                &mut result__,
            )
            .map(|| result__)
        }
    }
    pub(crate) fn SetIsChecked(&self, value: bool) -> windows_core::Result<()> {
        unsafe {
            (windows_core::Interface::vtable(self).SetIsChecked)(
                windows_core::Interface::as_raw(self),
                value,
            )
            .ok()
        }
    }
}
#[repr(C)]
pub struct IToggleMenuFlyoutItem_Vtbl {
    pub base__: windows_core::IInspectable_Vtbl,
    pub IsChecked:
        unsafe extern "system" fn(*mut core::ffi::c_void, *mut bool) -> windows_core::HRESULT,
    pub SetIsChecked:
        unsafe extern "system" fn(*mut core::ffi::c_void, bool) -> windows_core::HRESULT,
}
windows_core::imp::define_winrt_interface!(
    IToggleMenuFlyoutItemFactory,
    IToggleMenuFlyoutItemFactory_Vtbl,
    0x426dfd57_6cc9_570f_950d_37437235dc89
);
#[repr(C)]
pub struct IToggleMenuFlyoutItemFactory_Vtbl {
    pub base__: windows_core::IInspectable_Vtbl,
    pub CreateInstance: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
    ) -> windows_core::HRESULT,
}
#[repr(transparent)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioMenuFlyoutItem(windows_core::IUnknown);
windows_core::imp::interface_hierarchy!(
    RadioMenuFlyoutItem,
    windows_core::IUnknown,
    windows_core::IInspectable
);
windows_core::imp::required_hierarchy!(
    RadioMenuFlyoutItem,
    MenuFlyoutItem,
    MenuFlyoutItemBase,
    Control,
    FrameworkElement,
    UIElement,
    DependencyObject
);
impl RadioMenuFlyoutItem {
    pub(crate) fn new() -> windows_core::Result<Self> {
        Self::IRadioMenuFlyoutItemFactory(|this| unsafe {
            let mut result__ = core::mem::zeroed();
            (windows_core::Interface::vtable(this).CreateInstance)(
                windows_core::Interface::as_raw(this),
                core::ptr::null_mut(),
                &mut None::<windows_core::IInspectable> as *mut _ as _,
                &mut result__,
            )
            .and_then(|| windows_core::imp::from_abi(result__))
        })
    }
    fn IRadioMenuFlyoutItemFactory<
        R,
        F: FnOnce(&IRadioMenuFlyoutItemFactory) -> windows_core::Result<R>,
    >(
        callback: F,
    ) -> windows_core::Result<R> {
        static SHARED: windows_core::imp::FactoryCache<
            RadioMenuFlyoutItem,
            IRadioMenuFlyoutItemFactory,
        > = windows_core::imp::FactoryCache::new();
        SHARED.call(callback)
    }
}
impl windows_core::RuntimeType for RadioMenuFlyoutItem {
    const SIGNATURE: windows_core::imp::ConstBuffer =
        windows_core::imp::ConstBuffer::for_class::<Self, IRadioMenuFlyoutItem>();
}
unsafe impl windows_core::Interface for RadioMenuFlyoutItem {
    type Vtable = <IRadioMenuFlyoutItem as windows_core::Interface>::Vtable;
    const IID: windows_core::GUID = <IRadioMenuFlyoutItem as windows_core::Interface>::IID;
    const INSPECTABLE: bool = true;
}
impl core::ops::Deref for RadioMenuFlyoutItem {
    type Target = IRadioMenuFlyoutItem;
    fn deref(&self) -> &Self::Target {
        unsafe { core::mem::transmute(self) }
    }
}
impl windows_core::RuntimeName for RadioMenuFlyoutItem {
    const NAME: &'static str = "Microsoft.UI.Xaml.Controls.RadioMenuFlyoutItem";
}
unsafe impl Send for RadioMenuFlyoutItem {}
unsafe impl Sync for RadioMenuFlyoutItem {}
#[repr(transparent)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToggleMenuFlyoutItem(windows_core::IUnknown);
windows_core::imp::interface_hierarchy!(
    ToggleMenuFlyoutItem,
    windows_core::IUnknown,
    windows_core::IInspectable
);
windows_core::imp::required_hierarchy!(
    ToggleMenuFlyoutItem,
    MenuFlyoutItem,
    MenuFlyoutItemBase,
    Control,
    FrameworkElement,
    UIElement,
    DependencyObject
);
impl ToggleMenuFlyoutItem {
    pub(crate) fn new() -> windows_core::Result<Self> {
        Self::IToggleMenuFlyoutItemFactory(|this| unsafe {
            let mut result__ = core::mem::zeroed();
            (windows_core::Interface::vtable(this).CreateInstance)(
                windows_core::Interface::as_raw(this),
                core::ptr::null_mut(),
                &mut None::<windows_core::IInspectable> as *mut _ as _,
                &mut result__,
            )
            .and_then(|| windows_core::imp::from_abi(result__))
        })
    }
    fn IToggleMenuFlyoutItemFactory<
        R,
        F: FnOnce(&IToggleMenuFlyoutItemFactory) -> windows_core::Result<R>,
    >(
        callback: F,
    ) -> windows_core::Result<R> {
        static SHARED: windows_core::imp::FactoryCache<
            ToggleMenuFlyoutItem,
            IToggleMenuFlyoutItemFactory,
        > = windows_core::imp::FactoryCache::new();
        SHARED.call(callback)
    }
}
impl windows_core::RuntimeType for ToggleMenuFlyoutItem {
    const SIGNATURE: windows_core::imp::ConstBuffer =
        windows_core::imp::ConstBuffer::for_class::<Self, IToggleMenuFlyoutItem>();
}
unsafe impl windows_core::Interface for ToggleMenuFlyoutItem {
    type Vtable = <IToggleMenuFlyoutItem as windows_core::Interface>::Vtable;
    const IID: windows_core::GUID = <IToggleMenuFlyoutItem as windows_core::Interface>::IID;
    const INSPECTABLE: bool = true;
}
impl core::ops::Deref for ToggleMenuFlyoutItem {
    type Target = IToggleMenuFlyoutItem;
    fn deref(&self) -> &Self::Target {
        unsafe { core::mem::transmute(self) }
    }
}
impl windows_core::RuntimeName for ToggleMenuFlyoutItem {
    const NAME: &'static str = "Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem";
}
unsafe impl Send for ToggleMenuFlyoutItem {}
unsafe impl Sync for ToggleMenuFlyoutItem {}
