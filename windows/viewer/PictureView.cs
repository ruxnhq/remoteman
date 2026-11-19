using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;

namespace Remoteman;

/// The computer's picture: a SwapChainPanel the engine draws into, inside a control that takes
/// the keyboard, so every key (Tab, the arrows, Alt, F10, Space included) goes to the computer
/// rather than to XAML's focus and menus.
public sealed class PictureView : UserControl
{
    public readonly SwapChainPanel Panel = new();
    bool _attached;

    public PictureView()
    {
        IsTabStop = true;
        UseSystemFocusVisuals = false;
        AllowFocusOnInteraction = true;
        Content = Panel;
        Panel.SizeChanged += (_, _) => Tell();
        Panel.CompositionScaleChanged += (_, _) => Tell();
        Panel.LayoutUpdated += (_, _) => TellIfMoved();
        Panel.PointerMoved += (_, e) => Pointer(e);
        Panel.PointerPressed += (_, e) => { Focus(FocusState.Pointer); Panel.CapturePointer(e.Pointer); Pointer(e); e.Handled = true; };
        Panel.PointerReleased += (_, e) => { Pointer(e); Panel.ReleasePointerCapture(e.Pointer); e.Handled = true; };
        Panel.PointerCanceled += (_, e) => Pointer(e);
        Panel.PointerWheelChanged += (_, e) =>
        {
            var p = e.GetCurrentPoint(Panel).Properties;
            Engine.remoteman_wheel(p.MouseWheelDelta / 120f, p.IsHorizontalMouseWheel);
            e.Handled = true;
        };
        PreviewKeyDown += (_, e) => Key(e, true);
        PreviewKeyUp += (_, e) => Key(e, false);
        GotFocus += (_, _) => Engine.remoteman_focus(true);
        LostFocus += (_, _) => Engine.remoteman_focus(false);
        // No accelerators of XAML's own while the picture has the keyboard.
        ProcessKeyboardAccelerators += (_, e) => e.Handled = true;
        Loaded += (_, _) => Attach();
    }

    void Attach()
    {
        if (_attached || App.Window is null) return;
        Tell();
        var why = Engine.Attach(Panel, App.Window.Hwnd);
        if (why is not null) Log.Say($"the panel: {why}");
        _attached = why is null;
    }

    (int, int, int, int, float, float) _told;

    void TellIfMoved()
    {
        if (Measure() != _told) Tell();
    }

    (int, int, int, int, float, float) Measure()
    {
        if (XamlRoot is null) return default;
        var at = Panel.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(0, 0));
        double k = XamlRoot.RasterizationScale;
        float sx = Panel.CompositionScaleX, sy = Panel.CompositionScaleY;
        return ((int)Math.Round(at.X * k), (int)Math.Round(at.Y * k), (int)Math.Round(Panel.ActualWidth * sx), (int)Math.Round(Panel.ActualHeight * sy), sx, sy);
    }

    void Tell()
    {
        var m = Measure();
        if (m.Item3 <= 0 || m.Item4 <= 0) return;
        _told = m;
        Engine.remoteman_panel(m.Item1, m.Item2, m.Item3, m.Item4, m.Item5, m.Item6);
    }

    uint _buttons;

    void Pointer(PointerRoutedEventArgs e)
    {
        var pt = e.GetCurrentPoint(Panel);
        int x = (int)Math.Round(pt.Position.X * Panel.CompositionScaleX), y = (int)Math.Round(pt.Position.Y * Panel.CompositionScaleY);
        Engine.remoteman_pointer(x, y);
        var p = pt.Properties;
        uint now = (p.IsLeftButtonPressed ? 1u : 0) | (p.IsRightButtonPressed ? 2u : 0) | (p.IsMiddleButtonPressed ? 4u : 0);
        for (int b = 0; b < 3; b++)
        {
            uint bit = 1u << b;
            if ((now & bit) != (_buttons & bit)) Engine.remoteman_button(b, (now & bit) != 0, x, y);
        }
        _buttons = now;
        e.Handled = true;
    }

    void Key(KeyRoutedEventArgs e, bool down)
    {
        var s = e.KeyStatus;
        uint scancode = s.ScanCode;
        if (scancode == 0) scancode = Native.MapVirtualKeyW((uint)e.Key, 0);
        // A repeat: the key was down already.
        bool repeat = down && s.WasKeyDown;
        if (Engine.remoteman_key(scancode, s.IsExtendedKey, down, repeat)) e.Handled = true;
    }
}

static partial class Native
{
    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    public static partial uint MapVirtualKeyW(uint code, uint mapType);
}
