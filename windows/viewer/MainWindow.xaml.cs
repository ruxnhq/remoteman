using System.Text.Json.Nodes;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Remoteman;

/// The one window: the title bar (which carries the session's bar during a session), the page
/// the engine's state says, its dialogs, and a line of news at the bottom.
public sealed partial class MainWindow : Window
{
    public nint Hwnd => WinRT.Interop.WindowNative.GetWindowHandle(this);
    public JsonObject State = new();
    readonly Views _views;
    bool _fullscreen;

    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(TitleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        var (w, h) = App.Options.Window ?? (984, 644);
        AppWindow.Resize(new Windows.Graphics.SizeInt32(Scale(w), Scale(h)));
        if (App.Options.Pos is var (x, y)) AppWindow.Move(new Windows.Graphics.PointInt32(x, y));
        _views = new Views(this);
        TitleBar.LayoutUpdated += (_, _) => Passthrough();
        Picture.PointerAt += PointerOverPicture;
        _barAway.Tick += (_, _) =>
        {
            // Not while the pointer is on it or one of its menus is open.
            if (Floating.Child is SessionBar && (_overBar || Microsoft.UI.Xaml.Media.VisualTreeHelper.GetOpenPopupsForXamlRoot(Content.XamlRoot).Count > 0)) return;
            _barAway.Stop();
            Floating.Visibility = Visibility.Collapsed;
        };
        // The engine puts its own window over the picture for variable refresh, except while
        // the bar (and so its menus) is out.
        Floating.RegisterPropertyChangedCallback(UIElement.VisibilityProperty, (_, _) => Engine.Do("overlay", new { on = Floating.Visibility == Visibility.Visible }));
        Floating.PointerEntered += (_, _) => { _overBar = true; _barAway.Stop(); };
        Floating.PointerExited += (_, _) => { _overBar = false; _barAway.Start(); };
        Engine.Changed += Refresh;
        Closed += (_, _) =>
        {
            Engine.Changed -= Refresh;
            int code = Engine.Stop();
            Log.Say($"leaving with {code}");
            Environment.Exit(code);
        };
        var why = Engine.Start(App.Options.EngineArgs);
        if (why is not null)
        {
            Log.Say($"the engine didn't start: {why}");
            PageHost.Content = new TextBlock { Text = $"remoteman can't start: {why}", Margin = new Thickness(32), TextWrapping = TextWrapping.Wrap };
            return;
        }
        Log.Say("window: made, engine started");
        // The engine's first state; it connects to a computer named on the command line once
        // the picture's panel is attached (or straight away when there is none to attach).
        Refresh();
        _views.Start();
    }

    int Scale(int dips) => (int)Math.Round(dips * Native.GetDpiForWindow(Hwnd) / 96.0);

    public void Refresh()
    {
        State = Engine.State();
        foreach (var effect in State.A("effects")) Apply(effect!);
        _views.Show(State);
        string? toast = State["toast"]?.GetValue<string>();
        Toast.Message = toast ?? "";
        Toast.IsOpen = toast is not null;
    }

    void Apply(JsonNode e)
    {
        Log.Say($"effect: {e.ToJsonString()}");
        var presenter = AppWindow.Presenter as OverlappedPresenter;
        switch (e.S("do"))
        {
            case "minimise": presenter?.Minimize(); break;
            case "maximise":
                if (presenter?.State == OverlappedPresenterState.Maximized) presenter.Restore(); else presenter?.Maximize();
                break;
            case "close":
            case "quit": Close(); break;
            case "title": Title = e.S("text"); TitleText.Text = e.S("text").Replace(" - remoteman", ""); break;
            case "size":
                if (!_fullscreen && presenter?.State != OverlappedPresenterState.Maximized)
                    AppWindow.ResizeClient(new Windows.Graphics.SizeInt32(Scale((int)e.D("width")), Scale((int)e.D("height") + 40)));
                break;
            case "fullscreen": SetFullscreen(e.B("on")); break;
        }
    }

    public void SetFullscreen(bool on)
    {
        if (on == _fullscreen) return;
        _fullscreen = on;
        AppWindow.SetPresenter(on ? AppWindowPresenterKind.FullScreen : AppWindowPresenterKind.Overlapped);
        Engine.Do("now-fullscreen", new { on });
        Refresh();
        Log.Say($"window: {(on ? "full screen" : "out of full screen")}");
    }

    public void SetBack(bool shown) => BackButton.Visibility = shown ? Visibility.Visible : Visibility.Collapsed;

    bool _floating, _overBar;
    readonly DispatcherTimer _barAway = new() { Interval = TimeSpan.FromSeconds(2) };

    /// The session's bar (`null`: no session on screen): a pill over the top of the picture,
    /// always there, or brought out at the top edge in full screen and when Settings says so.
    /// The picture has the whole window behind it; the window's own buttons stay where Windows
    /// puts them, and the pill's words are what the window is moved by.
    public void SetSession(SessionBar? bar, JsonObject state)
    {
        bool floating = bar is not null && (_fullscreen || state.O("settings").B("hover_bar"));
        TitleRow.Height = new GridLength(bar is not null ? 0 : 40);
        if (bar is null)
        {
            Floating.Child = null;
            Floating.Visibility = Visibility.Collapsed;
            if (_dragOn is not null) { SetTitleBar(TitleBar); _dragOn = null; }
        }
        else
        {
            if (Floating.Child != bar) Floating.Child = bar;
            if (!floating) Floating.Visibility = Visibility.Visible;
            else if (!_floating) Floating.Visibility = Visibility.Collapsed;
            if (_dragOn != bar.DragArea) { SetTitleBar(bar.DragArea); _dragOn = bar.DragArea; }
        }
        BarHost.Content = null;
        // No title bar of Windows' own during a session in a window: the pill has the buttons.
        bool chrome = bar is null || _fullscreen;
        if (AppWindow.Presenter is OverlappedPresenter op && chrome != _chrome)
        {
            op.SetBorderAndTitleBar(true, chrome);
            _chrome = chrome;
        }
        if (bar is not null) bar.WindowButtons.Visibility = _fullscreen ? Visibility.Collapsed : Visibility.Visible;
        _floating = floating;
        if (bar is not null) Figures.Update(state); else Figures.Visibility = Visibility.Collapsed;
    }
    UIElement? _dragOn;
    bool _chrome = true;

    /// The pill's window buttons.
    public void WindowCommand(string what)
    {
        var op = AppWindow.Presenter as OverlappedPresenter;
        switch (what)
        {
            case "minimise": op?.Minimize(); break;
            case "maximise": if (op?.State == OverlappedPresenterState.Maximized) op.Restore(); else op?.Maximize(); break;
            case "close": Close(); break;
        }
    }

    /// The pointer over the picture, in the app's units: at the top edge the floating bar comes.
    void PointerOverPicture(double y)
    {
        if (!_floating || y > 3 || Floating.Visibility == Visibility.Visible) return;
        Floating.Visibility = Visibility.Visible;
        _barAway.Start();
    }

    /// Where the title bar takes clicks itself (its buttons) rather than moving the window.
    void Passthrough()
    {
        var rects = new List<Windows.Graphics.RectInt32>();
        double k = Content.XamlRoot?.RasterizationScale ?? 1;
        foreach (FrameworkElement e in new FrameworkElement[] { BackButton, BarHost })
        {
            if (e.Visibility != Visibility.Visible || e.ActualWidth <= 0 || (e == BarHost && BarHost.Content is null)) continue;
            var at = e.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(0, 0));
            rects.Add(new Windows.Graphics.RectInt32((int)(at.X * k), (int)(at.Y * k), (int)(e.ActualWidth * k), (int)(e.ActualHeight * k)));
        }
        string key = string.Join(";", rects.Select(r => $"{r.X},{r.Y},{r.Width},{r.Height}"));
        if (key == _passthrough) return;
        _passthrough = key;
        Microsoft.UI.Input.InputNonClientPointerSource.GetForWindowId(AppWindow.Id).SetRegionRects(Microsoft.UI.Input.NonClientRegionKind.Passthrough, rects.ToArray());
    }
    string _passthrough = "";
    /// A page, or the picture (`null`).
    public void SetBody(UIElement? page)
    {
        if (PageHost.Content != page) PageHost.Content = page;
        var picture = page is null ? Visibility.Visible : Visibility.Collapsed;
        if (Picture.Visibility != picture)
        {
            Picture.Visibility = picture;
            if (page is null) Picture.Focus(FocusState.Programmatic);
        }
    }

    void Back_Click(object sender, RoutedEventArgs e) => Engine.Do("back");
}

static partial class Native
{
    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    public static partial uint GetDpiForWindow(nint hwnd);
}
