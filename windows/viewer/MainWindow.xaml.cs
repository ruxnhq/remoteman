using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Remoteman;

public sealed partial class MainWindow : Window
{
    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(TitleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        var (w, h) = App.Options.Size ?? (1040, 700);
        AppWindow.Resize(new Windows.Graphics.SizeInt32(Scale(w), Scale(h)));
        if (App.Options.Pos is var (x, y)) AppWindow.Move(new Windows.Graphics.PointInt32(x, y));
        Pages.Navigate(typeof(HelloPage));
        if (App.Options.Seconds is int seconds)
        {
            var timer = DispatcherQueue.CreateTimer();
            timer.Interval = TimeSpan.FromSeconds(seconds);
            timer.Tick += (_, _) => { Log.Say("--seconds is up"); Close(); };
            timer.Start();
        }
        Activated += (_, e) => Log.Say($"window: {e.WindowActivationState}");
        Log.Say("window: made");
    }

    public nint Hwnd => WinRT.Interop.WindowNative.GetWindowHandle(this);

    int Scale(int dips) => (int)Math.Round(dips * Native.GetDpiForWindow(Hwnd) / 96.0);

    void Back_Click(object sender, RoutedEventArgs e)
    {
        if (Pages.CanGoBack) Pages.GoBack();
    }
}

static partial class Native
{
    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    public static partial uint GetDpiForWindow(nint hwnd);
}
