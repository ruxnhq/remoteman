using System.Net;
using System.Net.NetworkInformation;
using System.Net.Sockets;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace RemotemanHost;

/// The window (Sharing, Devices, About), the icon in the notification area, and the question
/// put to the owner when a new device wants to pair. Built in code from WinUI's own controls,
/// with the design's words (docs/design/windows/Host*.dc.html). Closing the window keeps the
/// icon; the host runs on whether or not this app does.
public sealed partial class MainWindow : Window
{
    readonly Tray _tray = new();
    readonly DispatcherTimer _tick = new() { Interval = TimeSpan.FromSeconds(1) };
    readonly string _pc = Environment.MachineName;
    Status? _status;
    bool _asked;
    string _page = "sharing";
    /// The fingerprint of the device whose question is showing, so it is asked once.
    string? _asking;
    ContentDialog? _question;
    long _answered = -1;
    bool _filling;

    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(TitleBar);
        AppWindow.Resize(new Windows.Graphics.SizeInt32(Scale(900), Scale(680)));
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "sharing.ico"));
        // Closing the window keeps the icon: the app goes on, only "Quit" ends it.
        AppWindow.Closing += (_, e) =>
        {
            e.Cancel = true;
            AppWindow.Hide();
            Log.Say("window: hidden (the icon stays)");
        };
        Nav.SelectionChanged += (_, e) =>
        {
            if (e.SelectedItem is NavigationViewItem item && item.Tag is string tag && tag != _page)
            {
                _page = tag;
                Fill();
            }
        };
        _tray.Open += () => ShowWindow(null);
        _tray.Menu = TrayMenu;
        _tick.Tick += (_, _) => Refresh();
        _tick.Start();
        Refresh();
        // (Refresh fills the page only when something changed: with no host, nothing did.)
        Fill();
        Log.Say("started");
    }

    int Scale(int dips)
    {
        uint dpi = GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this));
        return (int)Math.Round(dips * (dpi == 0 ? 96 : dpi) / 96.0);
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")] static extern uint GetDpiForWindow(nint hwnd);

    public void ShowWindow(string? page)
    {
        if (page is not null && page != _page)
        {
            _page = page;
            Fill();
        }
        Nav.SelectedItem = Nav.MenuItems.OfType<NavigationViewItem>().FirstOrDefault(i => (string)i.Tag == _page);
        AppWindow.Show();
        Activate();
    }

    /// Ask the host, and say so if it didn't take it.
    void Tell(string words)
    {
        var said = Host.Ask(words);
        Log.Say($"told the host: {words} -> {said ?? "(no host)"}");
        if (said is null) Toast("The host isn't running, so nothing was changed.");
        else if (said != "ok") Toast(said);
        Refresh();
    }

    void Toast(string text)
    {
        News.Message = text;
        News.IsOpen = true;
    }

    void Refresh()
    {
        var status = Host.Status();
        bool changed = !Same(status, _status);
        _status = status;
        // The icon and its tooltip.
        if (status is null) _tray.Show("paused", "remoteman: the host isn't running");
        else
        {
            var (title, lines) = status.Words(_pc);
            _tray.Show(status.Paused ? "paused" : status.Client is not null ? "connected" : "sharing", $"remoteman: {title}\n{string.Join("\n", lines)}");
        }
        // A device that wants to pair is put to the owner.
        if (status?.Pending is Pending p && _asking != p.Fingerprint) Ask(p);
        if (status?.Pending is null && _question is not null)
        {
            // Answered elsewhere (ctl), or its minute is up.
            _question.Hide();
            _question = null;
            _asking = null;
        }
        // The device answered for a file sent to it.
        if (status is not null)
        {
            if (_answered >= 0 && status.Answered > _answered)
            {
                string text = status.SentOk ? $"The device has the file: {status.Said}" : $"The file didn't reach the device: {status.Said}";
                Toast(text);
                _tray.Notify(status.SentOk ? "The device has the file" : "The file didn't reach the device", status.Said);
            }
            _answered = status.Answered;
        }
        // A device that just connected.
        if (status?.Client is Client c && _status is not null && _asked == false)
        {
            _tray.Notify($"{c.Name} connected", $"It has {(c.Placement == "mirror" ? "this PC's display" : c.Placement == "only" ? "the only display" : c.Placement == "main" ? "the main display" : "an extra display")} on {_pc}.");
        }
        _asked = status?.Client is not null;
        if (changed) Fill();
    }

    static bool Same(Status? a, Status? b)
    {
        if (a is null || b is null) return a is null && b is null;
        return a.Paused == b.Paused && a.Pairing == b.Pairing && a.Clipboard == b.Clipboard && a.Files == b.Files && a.Client == b.Client && a.Pending == b.Pending
            && a.Fingerprint == b.Fingerprint && a.Devices.SequenceEqual(b.Devices);
    }

    async void Ask(Pending p)
    {
        _asking = p.Fingerprint;
        _tray.Notify($"{p.Name} wants to pair", $"Its code should be {Host.Code(p.Code)}. Answer in Remoteman Host.");
        ShowWindow("devices");
        // Started in the background, the window has only just been shown: its content needs
        // a moment before a dialog can be put over it.
        for (int i = 0; i < 40 && Content.XamlRoot is null; i++) await Task.Delay(50);
        if (_asking != p.Fingerprint) return;
        var body = new StackPanel { Spacing = 12 };
        body.Children.Add(new TextBlock { Text = $"It shows {Host.Code(p.Code)} if it is the device in front of you.", TextWrapping = TextWrapping.Wrap });
        body.Children.Add(new TextBlock { Text = Host.Code(p.Code), FontSize = 40, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, CharacterSpacing = 120, HorizontalAlignment = HorizontalAlignment.Center });
        body.Children.Add(new TextBlock { Text = $"Its key: {Host.ShortKey(p.Fingerprint)}", Style = Style("CaptionTextBlockStyle"), Foreground = Brush("TextFillColorSecondaryBrush") });
        _question = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"Pair with {p.Name}?",
            Content = body,
            PrimaryButtonText = "Codes match",
            CloseButtonText = "Refuse",
            DefaultButton = ContentDialogButton.Close,
        };
        Log.Say($"asking: pair with {p.Name}? code {p.Code}");
        var shown = _question;
        var answer = await shown.ShowAsync();
        if (_question != shown) return;  // taken away: answered elsewhere
        _question = null;
        Tell(answer == ContentDialogResult.Primary ? "pair accept" : "pair decline");
        if (answer == ContentDialogResult.Primary) Toast($"{p.Name} was paired. Pairing has closed.");
    }

    List<(string, Action?)> TrayMenu()
    {
        var items = new List<(string, Action?)>();
        var s = _status;
        if (s is null)
        {
            items.Add(("Sharing couldn't start", null));
            items.Add(("The host isn't running.", null));
        }
        else
        {
            var (title, lines) = s.Words(_pc);
            items.Add((title, null));
            items.AddRange(lines.Select(l => (l, (Action?)null)));
            items.Add(("-", null));
            if (s.Client is not null)
            {
                if (s.Files) items.Add(("Send a file to the device…", () => _ = SendFile()));
                items.Add(("Disconnect", () => Tell("drop")));
            }
            items.Add(s.Paused ? ("Resume sharing", () => Tell("sharing on")) : ("Pause sharing", () => Tell("sharing off")));
            if (!s.Paused) items.Add(("Pair a device…", () => { Tell("pair on"); ShowWindow("devices"); }));
        }
        items.Add(("Settings…", () => ShowWindow("sharing")));
        items.Add(("-", null));
        items.Add(("Quit remoteman", Quit));
        return items;
    }

    void Quit()
    {
        Log.Say("quitting (the host runs on)");
        _tray.Dispose();
        _tick.Stop();
        Environment.Exit(0);
    }

    async Task SendFile()
    {
        var picker = new Windows.Storage.Pickers.FileOpenPicker();
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        picker.FileTypeFilter.Add("*");
        var file = await picker.PickSingleFileAsync();
        if (file is null) return;
        var said = Host.Ask($"send {file.Path}");
        Toast(said == "ok" ? "Sending the file" : said ?? "The host isn't running");
    }

    // ---- The pages ----

    static Style Style(string key) => (Style)Application.Current.Resources[key];
    static Brush Brush(string key) => (Brush)Application.Current.Resources[key];

    static TextBlock Heading(string text) => new() { Text = text, Style = Style("BodyStrongTextBlockStyle"), Margin = new Thickness(1, 24, 0, 6) };

    /// A card: WinUI's settings-row look (a thin layer over Mica, a hairline, rounded corners).
    static Border Card(UIElement content) => new()
    {
        Child = content,
        Background = Brush("CardBackgroundFillColorDefaultBrush"),
        BorderBrush = Brush("CardStrokeColorDefaultBrush"),
        BorderThickness = new Thickness(1),
        CornerRadius = new CornerRadius(6),
        Padding = new Thickness(16, 12, 16, 12),
        Margin = new Thickness(0, 0, 0, 2),
        MinHeight = 64,
    };

    /// A row: a title and a line under it on the left, something on the right.
    static Border Row(string title, string? subtitle, UIElement? right = null)
    {
        var grid = new Grid { ColumnSpacing = 16 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var words = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        words.Children.Add(new TextBlock { Text = title, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = false });
        if (!string.IsNullOrEmpty(subtitle))
            words.Children.Add(new TextBlock { Text = subtitle, Style = Style("CaptionTextBlockStyle"), Foreground = Brush("TextFillColorSecondaryBrush"), TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
        grid.Children.Add(words);
        if (right is FrameworkElement r)
        {
            r.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(r, 1);
            grid.Children.Add(r);
        }
        return Card(grid);
    }

    ToggleSwitch Switch(bool on, bool enabled, string name, Action<bool> changed)
    {
        var t = new ToggleSwitch { IsOn = on, IsEnabled = enabled, OnContent = "On", OffContent = "Off", MinWidth = 0 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(t, name);
        t.Toggled += (_, _) => { if (!_filling) changed(t.IsOn); };
        return t;
    }

    static Button Push(string text, Action act, bool accent = false)
    {
        var b = new Button { Content = text };
        if (accent) b.Style = Style("AccentButtonStyle");
        b.Click += (_, _) => act();
        return b;
    }

    void Fill()
    {
        _filling = true;
        Page.Children.Clear();
        var s = _status;
        switch (_page)
        {
            case "devices": FillDevices(s); break;
            case "about": FillAbout(s); break;
            default: FillSharing(s); break;
        }
        _filling = false;
    }

    void FillSharing(Status? s)
    {
        Page.Children.Add(new TextBlock { Text = "Sharing", Style = Style("TitleTextBlockStyle"), Margin = new Thickness(0, 8, 0, 8) });
        if (s is null)
        {
            Page.Children.Add(Row("Sharing couldn't start", "The host (remoteman-host.exe) isn't running, so devices can't connect. It runs elevated in your desktop session; start it, and this page follows."));
            return;
        }
        bool on = !s.Paused;
        Page.Children.Add(Row("Share this PC", on ? $"Sharing {_pc}. Paired devices can connect while it is on." : "Off: no device can connect.",
            Switch(on, true, "Share this PC", v => Tell(v ? "sharing on" : "sharing off"))));
        var (title, lines) = s.Words(_pc);
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        if (s.Client is not null)
        {
            if (s.Files) buttons.Children.Add(Push("Send a file…", () => _ = SendFile()));
            buttons.Children.Add(Push("Disconnect", () => Tell("drop")));
        }
        Page.Children.Add(Row(title, string.Join("\n", lines), buttons.Children.Count > 0 ? buttons : null));
        if (on)
        {
            if (s.Pairing)
                Page.Children.Add(Row("Pairing is open", "It accepts one new device, then closes. You are asked here before it is let in.", Push("Cancel", () => Tell("pair off"))));
            else
                Page.Children.Add(Row("Pair a device", "On the device, choose Pair a computer and pick this PC, or enter its address. You compare a six-digit code here.", Push("Pair a device…", () => Tell("pair on"), accent: true)));
        }
        Page.Children.Add(Heading("Getting in"));
        Page.Children.Add(Row("Before anyone has signed in", "The device would show Windows' sign-in screen and sign in there. Needs the background service, which isn't built yet.", new TextBlock { Text = "Not available yet", Foreground = Brush("TextFillColorDisabledBrush") }));
        Page.Children.Add(Heading("Picture"));
        Page.Children.Add(Row("Encoder", "On the graphics card: AV1 10-bit and HEVC 10-bit; HEVC 4:4:4 and HDR where the card's NVENC has them", new TextBlock { Text = "Automatic" }));
    }

    void FillDevices(Status? s)
    {
        Page.Children.Add(new TextBlock { Text = "Devices", Style = Style("TitleTextBlockStyle"), Margin = new Thickness(0, 8, 0, 8) });
        bool live = s is not null && !s.Paused;
        if (s?.Pairing == true)
            Page.Children.Add(Row("Pairing is open", "It accepts one new device, then closes. The device's code is shown here for you to compare.", Push("Cancel", () => Tell("pair off"))));
        else
        {
            var pair = Push("Pair a device…", () => Tell("pair on"), accent: true);
            pair.IsEnabled = live;
            Page.Children.Add(Row("Pair a device", live ? "Open pairing for one new device." : "Shown while sharing is on", pair));
        }

        Page.Children.Add(Heading("Paired devices"));
        if (s is null || s.Devices.Count == 0)
            Page.Children.Add(Row(s is null ? "Shown while the host is running" : "No device is paired yet", null));
        else
            foreach (var d in s.Devices)
            {
                var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                if (d.Connected) actions.Children.Add(Push("Disconnect", () => Tell("drop")));
                var device = d;
                actions.Children.Add(Push("Revoke…", () => _ = Revoke(device)));
                Page.Children.Add(Row(d.Name, d.Connected ? "Connected now" : "Paired", actions));
            }
        Page.Children.Add(new TextBlock { Text = "A revoked device has to be paired again before it can connect.", Style = Style("CaptionTextBlockStyle"), Foreground = Brush("TextFillColorSecondaryBrush"), Margin = new Thickness(1, 4, 0, 0) });

        Page.Children.Add(Heading("Clipboard and files"));
        Page.Children.Add(Row("Share the clipboard with devices", "Text and pictures copied on either side can be pasted on the other",
            Switch(s?.Clipboard ?? false, s is not null, "Share the clipboard with devices", v => Tell($"allow clipboard {(v ? "on" : "off")}"))));
        Page.Children.Add(Row("Receive files from devices", "Into Downloads, never over an existing file",
            Switch(s?.Files ?? false, s is not null, "Receive files from devices", v => Tell($"allow files {(v ? "on" : "off")}"))));

        Page.Children.Add(Heading("Address and key"));
        Page.Children.Add(Row("Address", s is null ? "Shown while the host is running" : Reach(s)));
        Page.Children.Add(Row("Key fingerprint", s is null ? "Shown while the host is running" : $"{Host.ShortKey(s.Fingerprint)}\nA device shows the same fingerprint once it is paired"));
    }

    void FillAbout(Status? s)
    {
        Page.Children.Add(new TextBlock { Text = "About", Style = Style("TitleTextBlockStyle"), Margin = new Thickness(0, 8, 0, 8) });
        Page.Children.Add(Row("remoteman host", $"This app shows and changes what the host on this PC does. The host (remoteman-host.exe) runs on its own: closing this window, or quitting, leaves it sharing as it is."));
        Page.Children.Add(Row("Host", s is null ? $"Not running (nothing answers on port {Host.Port})" : $"Running, listening on {s.Address}"));
        Page.Children.Add(Row("Microphone of a device", s?.Microphone == true ? "Played into Steam Streaming Microphone" : "Needs Steam's Streaming Microphone, which isn't installed"));
    }

    async Task Revoke(Device d)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"Revoke {d.Name}?",
            Content = "It is disconnected if it is connected now, and has to be paired again before it can connect.",
            PrimaryButtonText = "Revoke",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary) Tell($"revoke {d.Fingerprint}");
    }

    /// What to enter on a device: this PC's name on the network, and its addresses.
    string Reach(Status s)
    {
        string port = s.Address.Split(':').LastOrDefault() is string p && p != "7791" ? $":{p}" : "";
        var ips = NetworkInterface.GetAllNetworkInterfaces()
            .Where(n => n.OperationalStatus == OperationalStatus.Up && n.NetworkInterfaceType != NetworkInterfaceType.Loopback)
            .SelectMany(n => n.GetIPProperties().UnicastAddresses)
            .Where(a => a.Address.AddressFamily == AddressFamily.InterNetwork && !IPAddress.IsLoopback(a.Address))
            .Select(a => a.Address + port);
        var also = string.Join(", ", ips);
        return $"{Dns.GetHostName().ToLowerInvariant()}.local{port}" + (also.Length > 0 ? $"\nAlso: {also}" : "") + "\nWhat to enter on a device to reach this PC";
    }
}
