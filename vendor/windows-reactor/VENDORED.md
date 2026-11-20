# windows-reactor, vendored and patched

Microsoft's `windows-reactor` (declarative WinUI 3 for Rust, MIT OR Apache-2.0: `license-mit`,
`license-apache-2.0`), from the windows-rs repository at commit
`63a5e3d9b9afbd57c382d0a4f5a367f134e9dc25` (`crates/libs/reactor`). That commit, not the
crates.io release, because the release lacks the notification icon, reopenable windows and the
window handle. The workspace's `Cargo.toml` pins `windows-reactor` and `windows-reactor-setup`
to that commit and replaces `windows-reactor` with this copy through `[patch]`; this copy's own
`windows-*` dependencies point at the same commit.

It was copied from the owner's blurman project (its `reactor-spike` worktree,
`vendor/windows-reactor`), which made the first two patches. Each patched place says so in a
comment ("Spike patch" for blurman's, "remoteman patch" for ours).

1. **Backdrops that stay active** (blurman): `src/native/winui/window/active_backdrop.rs` (new)
   and `src/native/winui/window/mod.rs`. WinUI's `MicaBackdrop` and `DesktopAcrylicBackdrop`
   turn to a flat colour when another window is active; this drives `MicaController` /
   `DesktopAcrylicController` directly with a `SystemBackdropConfiguration` whose
   `IsInputActive` is always true, following the window's theme.
2. **More theme brushes** (blurman): `src/declaration.rs`, `ThemeBrush::SecondaryText`,
   `TertiaryText`, `SubtleSecondary`, `Divider`, `SystemCaution`, `SystemCautionBackground`,
   `SystemSuccess` (WinUI's `TextFillColorSecondaryBrush` and so on).
3. **Notifications from a notification icon** (remoteman): `ApplicationContext::notify(icon,
   title, text)` in `src/application.rs`, carried out in `src/native/app/application.rs`, with
   `NotifyIcon::notify` and `NIF_INFO` in `src/native/notifyicon/`. A balloon
   (`Shell_NotifyIconW(NIM_MODIFY)` with `NIF_INFO`), which Windows 10 and 11 show as a toast.
4. **The backdrop let go of when the window closes** (remoteman, fixing patch 1):
   `src/native/winui/window/active_backdrop.rs` removes its target and closes the controller in
   the window's `Closed` event, as the WinUI Gallery's backdrop sample does. Reactor tears a
   closed window's content down later, after WinUI's window has gone; the controller let go of
   then crashed the process (access violation in `Microsoft.ui.xaml.dll`). It only shows in an
   app that lives on after its window closes (a notification-area app).
5. **No `ContentDialog::Hide` on a closed window** (remoteman): `WinUiAdapter`'s `Drop`
   (`src/native/winui/mod.rs`) hides the dialogs still open only if one of its windows is still
   open (`NativeWindowState::is_closed`, `src/native/winui/window/mod.rs`). Hiding a dialog of a
   window that had closed with the dialog open crashed the same way.

Cargo.toml: the `[lints]` and docs.rs sections are gone (with `[lints.rust]` set here instead)
and the dependencies name the git commit instead of the windows-rs workspace. `public-api.txt` is
upstream's and does not list the patches.

To move to a newer Reactor: copy `crates/libs/reactor` from that commit over this folder, apply
the patches again (and check whether 4 and 5 are still needed: close a window with Mica, and one
with a dialog open, in an app that keeps running), and change the commit in the workspace's
`Cargo.toml` and here.
