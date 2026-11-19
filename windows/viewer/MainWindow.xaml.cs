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
        TitleRow.Height = new GridLength(on ? 0 : 40);
        Engine.Do("now-fullscreen", new { on });
        Log.Say($"window: {(on ? "full screen" : "out of full screen")}");
    }

    public void SetBack(bool shown) => BackButton.Visibility = shown ? Visibility.Visible : Visibility.Collapsed;
    public void SetBar(UIElement? bar)
    {
        if (BarHost.Content != bar) BarHost.Content = bar;
        Logo.Visibility = TitleText.Visibility = bar is null ? Visibility.Visible : Visibility.Collapsed;
    }
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
