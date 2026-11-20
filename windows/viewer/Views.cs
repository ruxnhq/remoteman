using System.Text.Json.Nodes;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Color = Windows.UI.Color;

namespace Remoteman;

/// Which page the engine's state calls for, and keeping each up to date. The words are the
/// design's (docs/design/windows); what each control does is the engine's to decide.
public sealed class Views
{
    readonly MainWindow _w;
    readonly ComputersPage _computers = new();
    readonly StatusPage _status = new();
    readonly SettingsPage _settings = new();
    readonly ComputerPage _computer = new();
    readonly SessionBar _bar = new();
    string _kind = "";

    public Views(MainWindow w) => _w = w;

    public void Start() { }

    public void Show(JsonObject state)
    {
        var screen = state.O("screen");
        string kind = screen.S("kind");
        if (kind != _kind) Log.Say($"screen: {kind}");
        bool fresh = kind != _kind;
        _kind = kind;
        _w.SetBack(kind is "settings" or "computer" or "problem");
        bool session = kind == "session" || (state.O("session") is not null && kind is "settings" or "computer");
        _bar.Update(state);
        _w.SetSession(kind == "session" ? _bar : null, state);
        switch (kind)
        {
            case "session":
                _w.SetBody(null);
                break;
            case "connecting":
            case "problem":
                _status.Update(state);
                _w.SetBody(_status);
                break;
            case "settings":
                _settings.Update(state, fresh);
                _w.SetBody(_settings);
                break;
            case "computer":
                _computer.Update(state, fresh);
                _w.SetBody(_computer);
                break;
            default:
                _computers.Update(state);
                _w.SetBody(_computers);
                break;
        }
        _ = session;
        Dialogs.Update(_w, state);
    }
}

/// The list of computers.
public sealed class ComputersPage : UserControl
{
    readonly Grid _cards = new() { ColumnSpacing = 12, RowSpacing = 12 };
    readonly StackPanel _nearby = new();
    readonly StackPanel _full = new() { Spacing = 20, Padding = new Thickness(32, 12, 32, 28) };
    readonly StackPanel _empty = new() { Spacing = 12, VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Center, MaxWidth = 420 };
    readonly Grid _root = new();
    string _shown = "";

    public ComputersPage()
    {
        for (int i = 0; i < 3; i++) _cards.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var head = new Grid { ColumnSpacing = 8 };
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.Children.Add(new TextBlock { Text = "Computers", Style = Ui.Style("TitleTextBlockStyle") });
        var pair = Ui.Button("Pair a computer", () => Engine.Do("pair"), accent: true, glyph: "");
        var settings = Ui.Square("", "Settings", () => Engine.Do("settings"));
        Grid.SetColumn(pair, 1); head.Children.Add(pair);
        Grid.SetColumn(settings, 2); head.Children.Add(settings);
        _full.Children.Add(head);
        var paired = new StackPanel { Spacing = 8 };
        paired.Children.Add(new TextBlock { Text = "Paired computers", Style = Ui.Style("BodyStrongTextBlockStyle") });
        paired.Children.Add(_cards);
        _full.Children.Add(paired);
        // Computers running remoteman on this network that this PC isn't paired with.
        var nearby = new StackPanel { Spacing = 8 };
        nearby.Children.Add(new TextBlock { Text = "Nearby", Style = Ui.Style("BodyStrongTextBlockStyle") });
        var nearbyCard = Ui.Card(_nearby);
        nearbyCard.Padding = new Thickness(0);
        nearby.Children.Add(nearbyCard);
        _full.Children.Add(nearby);
        _full.Children.Add(new TextBlock { Text = "A computer that is not listed can be paired by its address.", Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary });

        _empty.Children.Add(new FontIcon { Glyph = "", FontSize = 40, Foreground = Theme.Secondary });
        _empty.Children.Add(new TextBlock { Text = "No computers yet", Style = Ui.Style("SubtitleTextBlockStyle"), HorizontalAlignment = HorizontalAlignment.Center });
        _empty.Children.Add(new TextBlock { Text = "See and use another computer's screen from this one. Pair a Linux PC, a Mac or another Windows PC that runs the remoteman host.", TextWrapping = TextWrapping.Wrap, TextAlignment = TextAlignment.Center, Foreground = Theme.Secondary });
        var first = Ui.Button("Pair a computer", () => Engine.Do("pair"), accent: true, glyph: "");
        first.HorizontalAlignment = HorizontalAlignment.Center;
        _empty.Children.Add(first);
        var emptyHead = new Grid { Padding = new Thickness(32, 12, 32, 0), VerticalAlignment = VerticalAlignment.Top };
        emptyHead.Children.Add(new TextBlock { Text = "Computers", Style = Ui.Style("TitleTextBlockStyle") });
        var emptySettings = Ui.Square("", "Settings", () => Engine.Do("settings"));
        emptySettings.HorizontalAlignment = HorizontalAlignment.Right;
        emptyHead.Children.Add(emptySettings);
        _root.Children.Add(new ScrollViewer { Content = _full, HorizontalScrollMode = ScrollMode.Disabled, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled });
        _root.Children.Add(emptyHead);
        _root.Children.Add(_empty);
        Content = _root;
    }

    public void Update(JsonObject state)
    {
        var list = state.A("computers");
        var found = state.A("nearby");
        string key = list.ToJsonString() + found.ToJsonString();
        if (key == _shown) return;
        _shown = key;
        bool none = list.Count == 0 && found.Count == 0;
        _nearby.Children.Clear();
        foreach (var near in found)
        {
            string address = near!.S("address"), about = string.Join(" · ", new[] { near.S("os_name"), "on this network", "not paired" }.Where(x => x.Length > 0));
            var words = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
            words.Children.Add(new TextBlock { Text = near.S("name") });
            words.Children.Add(new TextBlock { Text = about, Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary });
            var pair = Ui.Button("Pair…", () => Engine.Do("pair", new { address }));
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(pair, $"Pair {near.S("name")}");
            var row = new Grid { ColumnSpacing = 14, Padding = new Thickness(16, 10, 16, 10), MinHeight = 60 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var icon = Ui.Icon("");
            icon.VerticalAlignment = VerticalAlignment.Center;
            row.Children.Add(icon);
            Grid.SetColumn(words, 1); row.Children.Add(words);
            Grid.SetColumn(pair, 2); pair.VerticalAlignment = VerticalAlignment.Center; row.Children.Add(pair);
            _nearby.Children.Add(row);
        }
        if (found.Count == 0)
            _nearby.Children.Add(new TextBlock { Text = "None found. A computer shows here while its remoteman host is on and it is on this network.", Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(16, 12, 16, 12) });
        ((UIElement)_root.Children[0]).Visibility = none ? Visibility.Collapsed : Visibility.Visible;
        ((UIElement)_root.Children[1]).Visibility = _empty.Visibility = none ? Visibility.Visible : Visibility.Collapsed;
        _cards.Children.Clear();
        _cards.RowDefinitions.Clear();
        int n = 0;
        foreach (var c in list)
        {
            if (n % 3 == 0) _cards.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            var card = Card(c!);
            Grid.SetColumn(card, n % 3);
            Grid.SetRow(card, n / 3);
            _cards.Children.Add(card);
            n++;
        }
    }

    static Color Thumb(string os) => os.ToLowerInvariant() switch
    {
        "linux" => Color.FromArgb(255, 0x1E, 0x2A, 0x44),
        "macos" or "mac" or "darwin" => Color.FromArgb(255, 0x3A, 0x35, 0x50),
        "windows" => Color.FromArgb(255, 0x24, 0x3B, 0x3A),
        _ => Color.FromArgb(255, 0x33, 0x33, 0x33),
    };

    static FrameworkElement Card(JsonNode c)
    {
        string address = c.S("address"), reach = c.S("reach");
        var tag = new Border { Background = new SolidColorBrush(Color.FromArgb(115, 0, 0, 0)), CornerRadius = new CornerRadius(4), Padding = new Thickness(8, 2, 8, 2), HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Bottom, Child = new TextBlock { Text = c.S("os_name"), FontSize = 12, Foreground = new SolidColorBrush(Colors.White) } };
        var thumb = new Border { Height = 112, CornerRadius = new CornerRadius(4), Background = new SolidColorBrush(Thumb(c.S("os"))), Padding = new Thickness(10), Child = tag };
        string state = reach switch { "unreachable" => "Not reachable", "asleep" => "Asleep", "online" => "Online", _ => "" };
        string used = c.S("last") is { Length: > 0 } l ? $"used {l}" : "never used";
        string line = c.B("connected") ? "Connected now" : state.Length > 0 ? $"{state} · {used}" : char.ToUpper(used[0]) + used[1..];
        var dot = new Microsoft.UI.Xaml.Shapes.Ellipse { Width = 8, Height = 8, Fill = reach switch { "online" => Theme.Ok, "unreachable" => Theme.Bad, "asleep" => Theme.Warn, _ => Theme.Secondary }, VerticalAlignment = VerticalAlignment.Center };
        var words = new StackPanel();
        words.Children.Add(new TextBlock { Text = c.S("name"), Style = Ui.Style("BodyStrongTextBlockStyle"), TextTrimming = TextTrimming.CharacterEllipsis });
        words.Children.Add(Ui.Row(6, dot, new TextBlock { Text = line, Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary }));
        // One that didn't answer and can be sent the waking packet is offered that.
        bool wake = reach == "unreachable" && c.B("wakeable");
        var connect = wake ? Ui.Button("Wake and connect", () => Engine.Do("wake", new { address })) : Ui.Button("Connect", () => Engine.Do("connect", new { address }), accent: true);
        connect.HorizontalAlignment = HorizontalAlignment.Stretch;
        var more = Ui.Square("", $"More for {c.S("name")}", null);
        var menu = new MenuFlyout();
        menu.Items.Add(Ui.Item("Settings for this computer…", () => Engine.Do("computer", new { address }), ""));
        menu.Items.Add(Ui.Item("Edit address…", () => Engine.Do("edit-address", new { address }), ""));
        menu.Items.Add(new MenuFlyoutSeparator());
        var forget = Ui.Item("Forget…", () => Engine.Do("forget", new { address }), "");
        forget.Foreground = Theme.Bad;
        menu.Items.Add(forget);
        more.Flyout = menu;
        var acts = new Grid { ColumnSpacing = 8 };
        acts.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        acts.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        acts.Children.Add(connect);
        Grid.SetColumn(more, 1); acts.Children.Add(more);
        var col = new StackPanel { Spacing = 12 };
        col.Children.Add(thumb);
        col.Children.Add(words);
        col.Children.Add(acts);
        var card = Ui.Card(col);
        card.Padding = new Thickness(12);
        card.DoubleTapped += (_, _) => Engine.Do("connect", new { address });
        return card;
    }
}

/// Connecting, and the pages that say why there is no session.
public sealed class StatusPage : UserControl
{
    readonly ProgressRing _ring = new() { Width = 40, Height = 40, IsActive = true };
    readonly FontIcon _sign = new() { FontSize = 40 };
    readonly TextBlock _title = new() { Style = Ui.Style("SubtitleTextBlockStyle"), TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap };
    readonly TextBlock _words = new() { TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap, Foreground = Theme.Secondary, MaxWidth = 460 };
    readonly StackPanel _acts = new() { Orientation = Orientation.Horizontal, Spacing = 8, HorizontalAlignment = HorizontalAlignment.Center };
    readonly PasswordBox _password = new() { Width = 300, PlaceholderText = "Password" };
    readonly CheckBox _remember = new() { Content = "Remember it on this PC" };
    readonly StackPanel _locked = new() { Spacing = 8, HorizontalAlignment = HorizontalAlignment.Center };
    string _shown = "";

    public StatusPage()
    {
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_password, "Password");
        _locked.Children.Add(_password);
        _locked.Children.Add(_remember);
        _password.KeyDown += (_, e) => { if (e.Key == Windows.System.VirtualKey.Enter) Unlock(); };
        var col = new StackPanel { Spacing = 16, VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Center, MaxWidth = 520 };
        col.Children.Add(_ring);
        col.Children.Add(_sign);
        col.Children.Add(_title);
        col.Children.Add(_words);
        col.Children.Add(_locked);
        col.Children.Add(_acts);
        Content = col;
    }

    void Unlock() => Engine.Do("unlock", new { password = _password.Password, remember = _remember.IsChecked == true });

    public void Update(JsonObject state)
    {
        var s = state.O("screen")!;
        string key = s.ToJsonString();
        if (key == _shown) return;
        _shown = key;
        string name = s.S("name") is { Length: > 0 } n ? n : s.S("address");
        string address = s.S("address");
        string host = address.Split(':')[0];
        _acts.Children.Clear();
        _locked.Visibility = Visibility.Collapsed;
        _ring.Visibility = Visibility.Collapsed;
        _sign.Visibility = Visibility.Visible;
        _sign.Foreground = Theme.Secondary;
        void Act(string text, Action act, bool accent = false) => _acts.Children.Add(Ui.Button(text, act, accent));
        void Sign(string glyph, Brush? brush = null) { _sign.Glyph = glyph; if (brush is not null) _sign.Foreground = brush; }
        if (s.S("kind") == "connecting")
        {
            _sign.Visibility = Visibility.Collapsed;
            _ring.Visibility = Visibility.Visible;
            _title.Text = $"Connecting to {name}";
            var m = state.A("monitor");
            _words.Text = m.Count == 3 ? $"Asking {name} for a {m[0]} × {m[1]} display at {m[2]} Hz…" : "";
            Act("Cancel", () => Engine.Do("cancel-connect"));
            return;
        }
        var p = s.O("problem");
        string said = p.S("said");
        switch (p.S("kind"))
        {
            case "unreachable":
                Sign("", Theme.Bad);
                _title.Text = $"Can't reach {name}";
                _words.Text = $"{said} at {host}. It may be off, on another network, or its firewall may be closed to remoteman.";
                Act("Try again", () => Engine.Do("connect", new { address }), true);
                if (state.A("computers").Any(x => x.S("address") == address && x.B("wakeable")))
                    Act("Wake and connect", () => Engine.Do("wake", new { address }));
                Act("Edit address…", () => Engine.Do("edit-address", new { address }));
                Act("Back", () => Engine.Do("back"));
                break;
            case "asleep":
                Sign("", Theme.Warn);
                _title.Text = $"{name} is asleep";
                _words.Text = "Waking it over the network takes a few seconds.";
                Act("Wake and connect", () => Engine.Do("wake", new { address }), true);
                Act("Back", () => Engine.Do("back"));
                break;
            case "not-paired":
                Sign("", Theme.Warn);
                _title.Text = $"{name} doesn't know this PC";
                _words.Text = "It answered, but this PC is no longer on its list of paired devices. Put it in pairing mode and pair again.";
                Act("Pair again…", () => Engine.Do("pair", new { address }), true);
                Act("Back", () => Engine.Do("back"));
                break;
            case "changed":
                Sign("", Theme.Bad);
                _title.Text = $"This isn't the {name} this PC paired with";
                _words.Text = $"Something else answered at {host}, with another key. If {name} was set up again, forget it and pair it again.";
                Act("Forget…", () => Engine.Do("forget", new { address }));
                Act("Back", () => Engine.Do("back"), true);
                break;
            case "refused":
                Sign("", Theme.Bad);
                _title.Text = $"{name} closed the connection";
                _words.Text = $"It answered, and said: {said}";
                Act("Connect again", () => Engine.Do("connect", new { address }), true);
                Act("Back", () => Engine.Do("back"));
                break;
            case "logging-in":
                _sign.Visibility = Visibility.Collapsed;
                _ring.Visibility = Visibility.Visible;
                _title.Text = $"{name} is logging in";
                _words.Text = said.Length > 0 ? $"It said: {said}. This PC tries again by itself." : "This PC tries again by itself.";
                Act("Try now", () => Engine.Do("connect", new { address }), true);
                Act("Back", () => Engine.Do("back"));
                break;
            case "locked":
                Sign("");
                _title.Text = $"{name} is locked";
                _words.Text = said;
                _locked.Visibility = Visibility.Visible;
                _password.Password = "";
                _remember.IsChecked = state.B("remember");
                Act("Unlock", Unlock, true);
                Act("Back", () => Engine.Do("back"));
                _password.Focus(FocusState.Programmatic);
                break;
            default:
                Sign("");
                _title.Text = $"The session with {name} ended";
                _words.Text = said;
                Act("Connect again", () => Engine.Do("connect", new { address }), true);
                Act("Back", () => Engine.Do("back"));
                break;
        }
    }
}

/// A settings page: groups of rows in cards, as WinUI's settings pages have them.
public abstract class RowsPage : UserControl
{
    protected readonly StackPanel Col = new() { Spacing = 18, Padding = new Thickness(40, 8, 40, 28), MaxWidth = 1000 };
    protected bool Quiet;

    protected RowsPage() => Content = new ScrollViewer { Content = Col, HorizontalScrollMode = ScrollMode.Disabled, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };

    protected StackPanel Group(string heading)
    {
        var rows = new StackPanel();
        var card = Ui.Card(rows);
        card.Padding = new Thickness(0);
        var sec = new StackPanel { Spacing = 8 };
        sec.Children.Add(new TextBlock { Text = heading, Style = Ui.Style("BodyStrongTextBlockStyle") });
        sec.Children.Add(card);
        Col.Children.Add(sec);
        return rows;
    }

    /// A row: its title, what it says under that, and its control; greyed with "Not available
    /// yet" where the product can't do it yet.
    protected TextBlock Row(StackPanel group, string title, string caption, FrameworkElement? control, bool on = true, string? notYet = null)
    {
        var words = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        words.Children.Add(new TextBlock { Text = title, TextWrapping = TextWrapping.Wrap });
        var cap = new TextBlock { Text = caption, Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary, TextWrapping = TextWrapping.Wrap, Visibility = caption.Length > 0 ? Visibility.Visible : Visibility.Collapsed };
        words.Children.Add(cap);
        var row = new Grid { ColumnSpacing = 14, Padding = new Thickness(16, 10, 16, 10), MinHeight = 60 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.Children.Add(words);
        if (!on)
        {
            words.Opacity = 0.5;
            var tag = new Border { BorderThickness = new Thickness(1), BorderBrush = Theme.Warn, CornerRadius = new CornerRadius(9), Padding = new Thickness(8, 1, 8, 1), VerticalAlignment = VerticalAlignment.Center, Child = new TextBlock { Text = notYet ?? "Not available yet", FontSize = 11, Foreground = Theme.Warn } };
            Grid.SetColumn(tag, 1);
            row.Children.Add(tag);
            if (control is Control c) c.IsEnabled = false;
        }
        if (control is not null)
        {
            if (Microsoft.UI.Xaml.Automation.AutomationProperties.GetName(control) is null or "")
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(control, title);
            control.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(control, 2);
            row.Children.Add(control);
        }
        if (group.Children.Count > 0)
            group.Children.Add(new Border { Height = 1, Background = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"] });
        group.Children.Add(row);
        return cap;
    }

    protected ToggleSwitch Toggle(bool on, Action flip)
    {
        var t = new ToggleSwitch { IsOn = on, OnContent = "", OffContent = "", MinWidth = 0 };
        t.Toggled += (_, _) => { if (!Quiet) flip(); };
        return t;
    }

    protected ComboBox Choice(JsonNode? options, Action<int> chose)
    {
        var box = new ComboBox { MinWidth = 200 };
        foreach (var item in options.A("items"))
            box.Items.Add(new ComboBoxItem { Content = item.S("text"), IsEnabled = item.B("on") });
        box.SelectedIndex = options.I("at");
        box.SelectionChanged += (_, _) => { if (!Quiet && box.SelectedIndex >= 0) chose(box.SelectedIndex); };
        return box;
    }

    protected static TextBlock Fixed(string text) => new() { Text = text, Foreground = Theme.Secondary };
}

/// Settings for every computer.
public sealed class SettingsPage : RowsPage
{
    string _shown = "";

    public void Update(JsonObject state, bool fresh)
    {
        var s = state.O("settings");
        var f = state.O("features");
        string key = s!.ToJsonString() + f!.ToJsonString();
        if (key == _shown) return;
        _shown = key;
        Quiet = true;
        Col.Children.Clear();
        var head = new StackPanel();
        head.Children.Add(new TextBlock { Text = "Settings", Style = Ui.Style("TitleTextBlockStyle") });
        head.Children.Add(new TextBlock { Text = "For every computer. One computer can differ: open its own settings from the list.", Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary });
        Col.Children.Add(head);
        void Flag(string flag) => Engine.Do("toggle", new { flag });
        void Choose(string choice, int index) => Engine.Do("choose", new { choice, index });

        var g = Group("Session window");
        Row(g, "Controls", "A title bar, or nothing but the picture with a bar that appears at the top edge", Choice(s.O("controls"), i => Choose("controls", i)));
        Row(g, "Start in full screen", "", Toggle(s.B("fullscreen"), () => Engine.Do("start-fullscreen")));

        g = Group("Picture");
        Row(g, "Default quality", "Automatic picks the best stream this PC decodes on its graphics card", Choice(s.O("default_quality"), i => Choose("default-quality", i)));
        Row(g, "Match this monitor's refresh rate", "Otherwise 60 Hz", Toggle(s.B("match_refresh"), () => Flag("match-refresh")), f.B("refresh_choice"));
        Row(g, "HDR", "Used when the computer offers it and this monitor is in HDR", Toggle(s.B("hdr"), () => Flag("hdr")), f.B("hdr"));

        g = Group("Keyboard");
        Row(g, "Send system shortcuts to the computer", "Alt+Tab, the Windows key and others, while a session is in front. Ctrl+Alt+Delete is always in Send keys.", Toggle(f.B("system_shortcuts") && s.B("shortcuts"), () => Flag("shortcuts")), f.B("system_shortcuts"));
        Row(g, "Release the keyboard", "Gives shortcuts back to this PC, and takes them again", Fixed("Ctrl+Alt+Shift"), f.B("release_shortcut"));
        Row(g, "On a Mac, Ctrl acts as Command", "Ctrl+C on this keyboard copies on a Mac, as it does here", Toggle(s.B("ctrl_as_command"), () => Flag("ctrl-as-command")), f.B("ctrl_as_command"));
        Row(g, "Windows key", "Opens the overview on a Linux computer, Spotlight's key on a Mac", Fixed("Super / Command"), f.B("key_mapping"));

        g = Group("Pointer and touch");
        Row(g, "Pointer", "Captured sends raw movement, which games and 3D apps need. Ctrl+Alt+Shift lets it go", Choice(s.O("pointer"), i => Choose("pointer", i)), f.B("pointer_capture"));
        Row(g, "Scroll direction", "", Choice(s.O("scroll"), i => Choose("scroll", i)), f.B("scroll_direction"));
        void Set(string what) => Engine.Do("set", new { what });
        Row(g, "Send touchscreen touches as touch", "Fingers arrive as fingers, so gestures on the computer work", Toggle(s.B("touch"), () => Set("touch")), f.B("touch"));

        g = Group("Sound, microphone and camera");
        Row(g, "Play the computer's sound here", "For computers paired from now on; each has its own switch", Toggle(s.B("sound"), () => Flag("sound")), f.B("sound"));
        Row(g, "Let computers use this microphone", "Still switched on by you in each session", Toggle(s.B("allow_microphone"), () => Set("allow-microphone")), f.B("microphone"));
        Row(g, "Let computers use this camera", "For calls made on the other computer", Toggle(s.B("allow_camera"), () => Set("allow-camera")), f.B("camera"));

        g = Group("Sharing");
        Row(g, "Share the clipboard", "Text and images copied on either side can be pasted on the other. For computers paired from now on", Toggle(s.B("clipboard"), () => Flag("clipboard")), f.B("clipboard"));
        Row(g, "Save received files to", s.S("folder"), Ui.Button("Change…", () => Engine.Do("pick-folder")), f.B("folder_choice"));

        var about = new HyperlinkButton { Content = "remoteman 0.1 · About", HorizontalAlignment = HorizontalAlignment.Left };
        about.Click += (_, _) => Engine.Do("about");
        Col.Children.Add(about);
        Quiet = false;
    }
}

/// Settings for one computer.
public sealed class ComputerPage : RowsPage
{
    string _shown = "";
    TextBox? _name;

    public void Update(JsonObject state, bool fresh)
    {
        string address = state.O("screen").S("address");
        var c = state.A("computers").FirstOrDefault(x => x.S("address") == address);
        if (c is null) return;
        var opts = state.O("computer");
        var f = state.O("features");
        // Not while a name is being typed: the page would lose it.
        string key = address + c.ToJsonString() + opts?.ToJsonString() + f!.ToJsonString() + state.O("settings").S("folder");
        if (key == _shown || (!fresh && _name?.FocusState is FocusState.Keyboard or FocusState.Pointer or FocusState.Programmatic)) return;
        _shown = key;
        Quiet = true;
        Col.Children.Clear();
        var head = new StackPanel();
        var crumb = new HyperlinkButton { Content = "Computers ›", Padding = new Thickness(0), FontSize = 12 };
        crumb.Click += (_, _) => Engine.Do("back");
        head.Children.Add(crumb);
        head.Children.Add(new TextBlock { Text = c.S("name"), Style = Ui.Style("TitleTextBlockStyle") });
        head.Children.Add(new TextBlock { Text = $"{c.S("os_name")} · paired {c.S("paired")} · last used {c.S("last")}", Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary });
        Col.Children.Add(head);
        void Choose(string choice, int index) => Engine.Do("choose", new { choice, address, index });

        var g = Group("Connection");
        _name = new TextBox { Text = c.S("name"), Width = 260 };
        _name.LostFocus += (_, _) => Engine.Do("rename", new { address, name = _name.Text });
        _name.KeyDown += (_, e) => { if (e.Key == Windows.System.VirtualKey.Enter) Engine.Do("rename", new { address, name = _name.Text }); };
        Row(g, "Name", "", _name);
        Row(g, "Address", c.S("short"), Ui.Button("Edit address…", () => Engine.Do("edit-address", new { address })));
        Row(g, "Wake it over the network", "When it is asleep and on the same network", Toggle(c.B("wake"), () => Engine.Do("set", new { what = "wake", address })), f.B("wake"));
        Row(g, "Getting in", "Joins your session there if you are logged in, otherwise shows its login screen", Fixed("The computer decides"), f.B("way_in"));

        g = Group("Display");
        string sits = c.S("placement") switch
        {
            "main" => "Its own displays stay on as secondary ones",
            "only" => "Its own screens turn off while you are connected",
            "mirror" => "Shows one of its own monitors as it is",
            _ => "Beside the computer's own displays, which stay on",
        };
        Row(g, "Where this display sits", sits, Choice(opts.O("placement"), i => Choose("placement", i)));
        Row(g, "Picture size", "How many pixels to ask the computer for", Choice(opts.O("size"), i => Choose("size", i)), f.B("follow_window"));
        Row(g, "A second display goes", "Where the computer puts a second display you add, among its other displays", Choice(opts.O("side"), i => Choose("side", i)), f.B("second_display"));
        Row(g, "Picture quality", "", Choice(opts.O("quality"), i => Choose("quality", i)));
        Row(g, "Bitrate limit", "", Choice(opts.O("bitrate"), i => Choose("bitrate", i)));

        g = Group("Sound and sharing");
        Row(g, "Play its sound here", "When a session starts; the bar's button is for the session", Toggle(c.B("sound"), () => Engine.Do("toggle", new { flag = "sound", address })), f.B("sound"));
        Row(g, "Share the clipboard with it", "When a session starts, if the computer allows it", Toggle(c.B("clipboard"), () => Engine.Do("toggle", new { flag = "clipboard", address })), f.B("clipboard"));
        Row(g, "Files it sends", $"Go into {state.O("settings").S("folder")}, never over a file that is there", null, f.B("files"));

        g = Group("Pairing");
        var keyLine = Row(g, "Its key", $"{c.S("key")} · the same is shown on {c.S("name")}", null);
        keyLine.FontFamily = new FontFamily("Consolas");
        var forget = Ui.Button("Forget…", () => Engine.Do("forget", new { address }));
        forget.Foreground = Theme.Bad;
        Row(g, "Forget this computer", "It has to be paired again before this PC can connect", forget);
        Quiet = false;
    }
}

/// The session's bar: in the title bar, or floating over the picture at the top edge.
public sealed class SessionBar : UserControl
{
    readonly TextBlock _name = new() { Style = Ui.Style("BodyStrongTextBlockStyle"), VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis };
    readonly TextBlock _format = new() { Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary, VerticalAlignment = VerticalAlignment.Center };
    readonly ToggleButton _sound = new() { Width = 36, Height = 32, Padding = new Thickness(0), Content = Ui.Icon("") };
    readonly ToggleButton _mic = new() { Width = 36, Height = 32, Padding = new Thickness(0), Content = Ui.Icon("") };
    readonly ToggleButton _camera = new() { Width = 36, Height = 32, Padding = new Thickness(0), Content = Ui.Icon("") };
    readonly Button _share = Ui.Square("", "Clipboard and files", null);
    JsonObject _state = new();
    bool _quiet;
    /// The part of the pill the window is moved by, and the window's own buttons.
    public readonly Grid DragArea;
    public readonly StackPanel WindowButtons;

    public SessionBar()
    {
        var keys = Ui.Drop("", "Send keys", KeysMenu);
        var display = Ui.Drop("", "Display", DisplayMenu);
        var picture = Ui.Drop("", "Picture", PictureMenu);
        ToolTipService.SetToolTip(_sound, "Play the computer's sound here");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_sound, "Play the computer's sound here");
        _sound.Click += (_, _) => { if (!_quiet) Engine.Do("sound"); };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_mic, "Microphone");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_camera, "Camera");
        _mic.Click += (_, _) => { if (!_quiet) Engine.Do("mic", new { on = _mic.IsChecked == true }); };
        _camera.Click += (_, _) => { if (!_quiet) Engine.Do("camera", new { on = _camera.IsChecked == true }); };
        _share.Flyout = Ui.Lazy(ShareMenu);
        var more = Ui.Square("", "More", null);
        more.Flyout = Ui.Lazy(MoreMenu);
        var full = Ui.Square("", "Full screen (F11)", () => Engine.Do("fullscreen"));
        var off = Ui.Button("Disconnect", () => Engine.Do("disconnect"), glyph: "");
        // The words are where the window is dragged from; the buttons take clicks.
        var words = Ui.Row(10, _name, _format);
        words.Padding = new Thickness(10, 0, 6, 0);
        words.MaxWidth = 340;
        DragArea = new Grid { Background = new SolidColorBrush(Colors.Transparent), Children = { words }, VerticalAlignment = VerticalAlignment.Stretch };
        // The window's own buttons sit in the pill during a session: Windows' title bar is
        // taken away then (MainWindow.SetSession), so they would otherwise be lost over the
        // picture.
        var min = Ui.Square("\uE921", "Minimise", () => App.Window?.WindowCommand("minimise"));
        var max = Ui.Square("\uE922", "Maximise", () => App.Window?.WindowCommand("maximise"));
        var close = Ui.Square("\uE8BB", "Close", () => App.Window?.WindowCommand("close"));
        foreach (var b in new[] { min, max, close }) b.Background = new SolidColorBrush(Colors.Transparent);
        WindowButtons = Ui.Row(0, min, max, close);
        var tools = Ui.Row(4, off, keys, display, picture, _sound, _mic, _camera, _share, more, full, WindowButtons);
        tools.VerticalAlignment = VerticalAlignment.Center;
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        row.Children.Add(DragArea);
        row.Children.Add(tools);
        Content = row;
    }

    public void Update(JsonObject state)
    {
        _state = state;
        var s = state.O("session");
        if (s is null) return;
        _name.Text = s.S("name");
        var f = s["format"] as JsonArray;
        _format.Text = f is { Count: 3 } ? $"{f[0]} × {f[1]} · {f[2]} Hz" : "";
        _quiet = true;
        _sound.IsChecked = s.B("sound");
        _sound.IsEnabled = Offers("sound") && state.O("features").B("sound");
        // The engine says whether each may be lent and, if not, why (the tip).
        _mic.IsChecked = s.B("mic");
        _mic.IsEnabled = s.B("mic_can") || s.B("mic");
        ToolTipService.SetToolTip(_mic, s.S("mic_why") is { Length: > 0 } micWhy ? micWhy : s.B("mic") ? $"Microphone, on: {s.S("mic_device")}" : "Microphone, off");
        _camera.IsChecked = s.B("camera");
        _camera.IsEnabled = s.B("camera_can") || s.B("camera");
        ToolTipService.SetToolTip(_camera, s.S("camera_why") is { Length: > 0 } camWhy ? camWhy : s.B("camera") ? "Camera, on" : "Camera, off");
        _share.IsEnabled = Offers("clipboard") || Offers("files");
        _quiet = false;
    }

    bool Offers(string feature) => _state.O("session").A("features").Any(x => x?.GetValue<string>() == feature);

    void KeysMenu(MenuFlyout m)
    {
        (string, string, string)[] chords =
        {
            ("Ctrl+Alt+Delete", "Windows can't pass this one on", "ctrl-alt-del"),
            ("Win", "Super on Linux, Command on a Mac", "win"),
            ("Win+L", "Locks the other computer", "win-l"),
            ("Alt+Tab", "", "alt-tab"),
            ("Alt+F4", "", "alt-f4"),
            ("Ctrl+Shift+Esc", "", "ctrl-shift-esc"),
            ("Print Screen", "", "print"),
        };
        foreach (var (label, hint, keys) in chords)
        {
            var i = Ui.Item(label, () => Engine.Do("keys", new { keys }));
            if (hint.Length > 0) ToolTipService.SetToolTip(i, hint);
            m.Items.Add(i);
        }
        m.Items.Add(new MenuFlyoutSeparator());
        var f = _state.O("features");
        var send = new ToggleMenuFlyoutItem { Text = "Send system shortcuts to the computer", IsChecked = _state.O("settings").B("shortcuts"), IsEnabled = f.B("system_shortcuts") };
        send.Click += (_, _) => Engine.Do("toggle", new { flag = "shortcuts" });
        m.Items.Add(send);
        m.Items.Add(new MenuFlyoutItem { Text = "Release the keyboard: Ctrl+Alt+Shift", IsEnabled = false });
    }

    void DisplayMenu(MenuFlyout m)
    {
        var s = _state.O("session");
        string now = s.S("placement");
        m.Items.Add(new MenuFlyoutItem { Text = "Where this display sits", IsEnabled = false });
        foreach (var (key, text, hint) in new[] { ("extend", "Extra display", "Beside the computer's own displays, which stay on"), ("main", "Main display", "Its own displays stay on as secondary ones"), ("only", "Only display", "Its own screens turn off while you are connected") })
        {
            var r = new RadioMenuFlyoutItem { Text = text, GroupName = "place", IsChecked = now == key, IsEnabled = Offers("placement") || key == "extend" };
            ToolTipService.SetToolTip(r, hint);
            r.Click += (_, _) => Engine.Do("place", new { placement = key });
            m.Items.Add(r);
        }
        var mirror = new RadioMenuFlyoutItem { Text = $"Mirror a monitor of {s.S("name")}", GroupName = "place", IsChecked = now == "mirror", IsEnabled = Offers("mirror") && _state.O("features").B("mirror") };
        mirror.Click += (_, _) => Engine.Do("place", new { placement = "mirror" });
        m.Items.Add(mirror);
        m.Items.Add(new MenuFlyoutSeparator());
        var size = new MenuFlyoutSubItem { Text = "Size", IsEnabled = _state.O("features").B("follow_window") };
        var opts = _state.O("computer").O("size");
        int n = 0;
        foreach (var item in opts.A("items"))
        {
            int index = n++;
            var r = new RadioMenuFlyoutItem { Text = item.S("text"), GroupName = "size", IsChecked = index == opts.I("at") };
            r.Click += (_, _) => Engine.Do("choose", new { choice = "size", address = s.S("address"), index });
            size.Items.Add(r);
        }
        m.Items.Add(size);
        var mon = _state.A("monitor");
        m.Items.Add(new MenuFlyoutItem { Text = $"Refresh rate: this monitor's, {(mon.Count == 3 ? mon[2] : 60)} Hz", IsEnabled = false });
        // The engine says what each comes to now ("picture"); these only show it and flip it.
        var pic = _state.O("picture");
        var hdr = new ToggleMenuFlyoutItem { Text = "HDR", IsChecked = _state.O("settings").B("hdr"), IsEnabled = _state.O("features").B("hdr") && Offers("hdr") && pic.O("hdr").B("screen") };
        ToolTipService.SetToolTip(hdr, "When the computer offers it and this monitor is in HDR. Now: " + pic.O("hdr").S("why"));
        hdr.Click += (_, _) => Engine.Do("toggle", new { flag = "hdr" });
        m.Items.Add(hdr);
        var vrr = new ToggleMenuFlyoutItem { Text = _state.O("features").B("variable_refresh") ? "Variable refresh rate" : "Variable refresh rate (not available yet)", IsChecked = _state.O("features").B("variable_refresh") && _state.O("settings").B("variable_refresh"), IsEnabled = _state.O("features").B("variable_refresh") && pic.O("variable_refresh")?["range"] is JsonArray && pic.O("variable_refresh").B("tearing_allowed") };
        ToolTipService.SetToolTip(vrr, "Shows each frame when it arrives, in full screen. Now: " + pic.O("variable_refresh").S("why"));
        vrr.Click += (_, _) => Engine.Do("toggle", new { flag = "variable-refresh" });
        m.Items.Add(vrr);
        m.Items.Add(new MenuFlyoutSeparator());
        bool second = s.B("second");
        var add = Ui.Item(second ? "Remove the second display" : "Add a second display…", () => Engine.Do("second"), "");
        add.IsEnabled = Offers("displays") && _state.O("features").B("second_display");
        m.Items.Add(add);
    }

    void PictureMenu(MenuFlyout m)
    {
        var s = _state.O("session");
        var d = _state.O("decodes");
        string now = s.S("quality");
        foreach (var (key, text, on) in new[] { ("automatic", "Automatic", true), ("sharp", "Sharp text (4:4:4)", _state.O("features").B("sharp_text") && d.B("hevc444")), ("hevc", "HEVC", d.B("hevc")), ("av1", "AV1", d.B("av1")) })
        {
            var r = new RadioMenuFlyoutItem { Text = text, GroupName = "quality", IsChecked = now == key, IsEnabled = on };
            r.Click += (_, _) => Engine.Do("pick", new { quality = key });
            m.Items.Add(r);
        }
        m.Items.Add(new MenuFlyoutSeparator());
        int[] steps = _state.A("bitrates").Select(x => (int)x!.GetValue<double>()).ToArray();
        if (steps.Length == 0) return;
        int step = s.I("bitrate_step");
        string Limit(int i) => steps[i] == 0 ? "No limit" : $"Up to {steps[i]} Mb/s";
        var label = new TextBlock { Text = $"Bitrate limit: {Limit(step)}" };
        var slider = new Slider { Minimum = 0, Maximum = steps.Length - 1, StepFrequency = 1, Value = step, Width = 240, IsEnabled = Offers("configure") };
        slider.ValueChanged += (_, e) => { label.Text = $"Bitrate limit: {Limit((int)e.NewValue)}"; Engine.Do("bitrate", new { step = (int)e.NewValue }); };
        var box = new StackPanel { Padding = new Thickness(12, 4, 12, 4) };
        box.Children.Add(label);
        box.Children.Add(slider);
        box.Children.Add(new TextBlock { Text = $"Now {s.O("figures").D("megabits"):0} Mb/s", Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary });
        m.Items.Add(new MenuFlyoutItemHost(box));
        m.Items.Add(new MenuFlyoutSeparator());
        var figs = new ToggleMenuFlyoutItem { Text = "Show figures", IsChecked = _state.O("settings").B("figures"), KeyboardAcceleratorTextOverride = "Ctrl+Alt+Shift+I" };
        figs.Click += (_, _) => Engine.Do("figures");
        m.Items.Add(figs);
    }

    void ShareMenu(MenuFlyout m)
    {
        var s = _state.O("session");
        var clip = new ToggleMenuFlyoutItem { Text = "Share the clipboard", IsChecked = s.B("clipboard"), IsEnabled = Offers("clipboard") };
        ToolTipService.SetToolTip(clip, "Text and images copied on either side can be pasted on the other");
        clip.Click += (_, _) => Engine.Do("clipboard");
        m.Items.Add(clip);
        var send = Ui.Item("Send a file…", () => Engine.Do("send-file"), "");
        send.IsEnabled = Offers("files");
        m.Items.Add(send);
        m.Items.Add(new MenuFlyoutItem { Text = $"Received files: saved to {_state.O("settings").S("folder")}, never over an existing file", IsEnabled = false });
    }

    void MoreMenu(MenuFlyout m)
    {
        string address = _state.O("session").S("address");
        m.Items.Add(Ui.Item("Settings for this computer…", () => Engine.Do("computer", new { address }), ""));
        m.Items.Add(Ui.Item("Settings", () => Engine.Do("settings")));
        m.Items.Add(new MenuFlyoutSeparator());
        bool hover = _state.O("settings").B("hover_bar");
        var bar = new RadioMenuFlyoutItem { Text = "Title bar", GroupName = "bar", IsChecked = !hover };
        bar.Click += (_, _) => Engine.Do("bar-mode", new { hover = false });
        var hide = new RadioMenuFlyoutItem { Text = "Show the bar on hover", GroupName = "bar", IsChecked = hover };
        hide.Click += (_, _) => Engine.Do("bar-mode", new { hover = true });
        m.Items.Add(bar);
        m.Items.Add(hide);
        m.Items.Add(new MenuFlyoutSeparator());
        m.Items.Add(Ui.Item("About remoteman", () => Engine.Do("about")));
    }
}

/// A piece of XAML inside a menu (the bitrate slider).
public sealed class MenuFlyoutItemHost : MenuFlyoutItem
{
    public MenuFlyoutItemHost(UIElement content)
    {
        Template = (ControlTemplate)Microsoft.UI.Xaml.Markup.XamlReader.Load(
            "<ControlTemplate xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation' TargetType='MenuFlyoutItem'><ContentPresenter Content='{TemplateBinding Tag}' /></ControlTemplate>");
        Tag = content;
    }
}

/// The figures over the picture, as the design has them.
public sealed class FiguresPanel : UserControl
{
    readonly Grid _grid = new() { ColumnSpacing = 16, RowSpacing = 2 };

    public FiguresPanel()
    {
        _grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Content = new Border
        {
            Child = _grid,
            Padding = new Thickness(14, 10, 14, 10),
            CornerRadius = new CornerRadius(8),
            Background = new SolidColorBrush(Color.FromArgb(200, 20, 20, 20)),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Top,
            Margin = new Thickness(12, 60, 12, 12),
        };
        IsHitTestVisible = false;
    }

    public void Update(JsonObject state)
    {
        var f = state.O("session").O("figures");
        bool on = state.O("settings").B("figures") && f is not null;
        Visibility = on ? Visibility.Visible : Visibility.Collapsed;
        if (!on) return;
        (string, string)[] lines =
        {
            ("FRAMES PER SECOND", $"{f.I("shown")}"),
            ("DECODED", $"{f.I("decoded")} a second"),
            ("PICTURE AGE", $"{f.D("age_ms"):0} ms"),
            ("ROUND TRIP", $"{f.D("round_trip_ms"):0.0} ms"),
            ("BITRATE", $"{f.D("megabits"):0} Mb/s"),
            ("LOST FRAMES", $"{f.I("lost")}"),
            ("REPLACED UNSEEN", $"{f.I("replaced")}"),
            ("DECODER", f.S("decoder")),
        };
        _grid.Children.Clear();
        _grid.RowDefinitions.Clear();
        for (int i = 0; i < lines.Length; i++)
        {
            _grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            var k = new TextBlock { Text = lines[i].Item1, FontSize = 11, Foreground = new SolidColorBrush(Color.FromArgb(255, 200, 200, 200)) };
            var v = new TextBlock { Text = lines[i].Item2, FontSize = 12, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Foreground = new SolidColorBrush(Colors.White) };
            Grid.SetRow(k, i); Grid.SetRow(v, i); Grid.SetColumn(v, 1);
            _grid.Children.Add(k); _grid.Children.Add(v);
        }
    }
}

/// Small helpers for building pages in code.
static class Ui
{
    public static Style Style(string name) => (Style)Application.Current.Resources[name];
    public static FontIcon Icon(string glyph) => new() { Glyph = glyph, FontSize = 16 };

    public static StackPanel Row(double spacing, params UIElement[] items)
    {
        var p = new StackPanel { Orientation = Orientation.Horizontal, Spacing = spacing };
        foreach (var i in items) p.Children.Add(i);
        return p;
    }

    public static Button Button(string text, Action act, bool accent = false, string? glyph = null)
    {
        var b = new Button { Content = glyph is null ? text : Row(8, Icon(glyph), new TextBlock { Text = text }) };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, text);
        if (accent) b.Style = Style("AccentButtonStyle");
        b.Click += (_, _) => act();
        return b;
    }

    public static Button Square(string glyph, string tip, Action? act)
    {
        var b = new Button { Content = Icon(glyph), Width = 32, Height = 32, Padding = new Thickness(0) };
        ToolTipService.SetToolTip(b, tip);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, tip);
        if (act is not null) b.Click += (_, _) => act();
        return b;
    }

    /// A button with words and a menu, the menu made afresh each time it opens.
    public static DropDownButton Drop(string glyph, string text, Action<MenuFlyout> fill)
    {
        var b = new DropDownButton { Content = Row(8, Icon(glyph), new TextBlock { Text = text }), Flyout = Lazy(fill) };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, text);
        return b;
    }

    public static MenuFlyout Lazy(Action<MenuFlyout> fill)
    {
        var m = new MenuFlyout();
        m.Opening += (_, _) => { m.Items.Clear(); fill(m); };
        return m;
    }

    public static MenuFlyoutItem Item(string text, Action act, string? glyph = null)
    {
        var i = new MenuFlyoutItem { Text = text };
        if (glyph is not null) i.Icon = new FontIcon { Glyph = glyph };
        i.Click += (_, _) => act();
        return i;
    }

    public static Border Card(UIElement child) => new()
    {
        Child = child,
        Padding = new Thickness(16, 10, 16, 10),
        CornerRadius = new CornerRadius(6),
        BorderThickness = new Thickness(1),
        Background = (Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"],
        BorderBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
    };
}

static class Theme
{
    static Brush R(string key) => (Brush)Application.Current.Resources[key];
    public static Brush Secondary => R("TextFillColorSecondaryBrush");
    public static Brush Ok => R("SystemFillColorSuccessBrush");
    public static Brush Warn => R("SystemFillColorCautionBrush");
    public static Brush Bad => R("SystemFillColorCriticalBrush");
}

/// The dialogs the state calls for, as ContentDialogs, one at a time.
static class Dialogs
{
    static ContentDialog? _open, _next;
    static string _kind = "";
    static DispatcherTimer? _countdown;

    public static void Update(MainWindow w, JsonObject state)
    {
        var d = state.O("dialog");
        string kind = d is null ? "" : d.S("kind") + "|" + d.S("waiting") + "|" + d.S("problem") + "|" + d.S("code");
        if (kind == _kind)
        {
            return;
        }
        _kind = kind;
        _countdown?.Stop();
        _next = null;
        _open?.Hide();
        if (d is null) return;
        var dialog = new ContentDialog { XamlRoot = w.Content.XamlRoot, Style = Ui.Style("DefaultContentDialogStyle") };
        bool cancelSent = false;
        switch (d.S("kind"))
        {
            case "pair":
            {
                var box = new TextBox { Header = "Address", PlaceholderText = "workshop.local or 192.168.1.20", Width = 360, Text = d.S("address") };
                var col = new StackPanel { Spacing = 12 };
                col.Children.Add(new TextBlock { Text = "Put the other computer in pairing mode, then enter its address here.", TextWrapping = TextWrapping.Wrap });
                col.Children.Add(box);
                col.Children.Add(new TextBlock { Text = "A name or an IP address. Add a colon and the port if it is not 7791.", Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary, TextWrapping = TextWrapping.Wrap });
                if (d.S("waiting") is { Length: > 0 } waiting)
                {
                    col.Children.Add(Ui.Row(8, new ProgressRing { Width = 16, Height = 16, IsActive = true }, new TextBlock { Text = $"Waiting for {waiting}…" }));
                    box.IsEnabled = false;
                    dialog.IsPrimaryButtonEnabled = false;
                }
                if (d.S("problem") is { Length: > 0 } problem) col.Children.Add(new InfoBar { IsOpen = true, IsClosable = false, Severity = InfoBarSeverity.Error, Message = problem });
                dialog.Title = "Pair a computer";
                dialog.Content = col;
                dialog.PrimaryButtonText = "Pair";
                dialog.CloseButtonText = "Cancel";
                dialog.DefaultButton = ContentDialogButton.Primary;
                dialog.PrimaryButtonClick += (_, e) => { e.Cancel = true; Engine.Do("pair-go", new { address = box.Text }); };
                dialog.Opened += (_, _) => box.Focus(FocusState.Programmatic);
                break;
            }
            case "code":
            {
                string name = d.S("name");
                var col = new StackPanel { Spacing = 12 };
                col.Children.Add(new TextBlock { Text = $"Check that {name} shows the same code. If it shows a different one, cancel: something else answered at that address.", TextWrapping = TextWrapping.Wrap });
                string code = d.S("code");
                col.Children.Add(new TextBlock { Text = code.Length == 6 ? code[..3] + " " + code[3..] : code, FontSize = 40, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, HorizontalAlignment = HorizontalAlignment.Center, CharacterSpacing = 100 });
                var left = new TextBlock { Foreground = Theme.Secondary, HorizontalAlignment = HorizontalAlignment.Center };
                int seconds = d.I("seconds");
                var until = DateTime.Now.AddSeconds(seconds);
                void Tick() { var l = until - DateTime.Now; if (l < TimeSpan.Zero) l = TimeSpan.Zero; left.Text = $"Pairing closes in {(int)l.TotalMinutes}:{l.Seconds:00}"; }
                Tick();
                _countdown = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
                _countdown.Tick += (_, _) => Tick();
                _countdown.Start();
                col.Children.Add(left);
                col.Children.Add(new TextBlock { Text = $"Confirm it on {name}. After pairing, this PC and {name} keep each other's key. Nothing else has to be typed again.", Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary, TextWrapping = TextWrapping.Wrap });
                dialog.Title = $"Pair with {name}?";
                dialog.Content = col;
                dialog.CloseButtonText = "Cancel";
                break;
            }
            case "paired":
                dialog.Title = $"Paired with {d.S("name")}";
                var keyText = new TextBlock { Text = d.S("key"), FontFamily = new FontFamily("Consolas"), FontSize = 16 };
                var pc = new StackPanel { Spacing = 8 };
                pc.Children.Add(new TextBlock { Text = $"This PC and {d.S("name")} keep each other's key. Its key:", TextWrapping = TextWrapping.Wrap });
                pc.Children.Add(keyText);
                dialog.Content = pc;
                dialog.PrimaryButtonText = "Connect";
                dialog.CloseButtonText = "Not now";
                dialog.DefaultButton = ContentDialogButton.Primary;
                dialog.PrimaryButtonClick += (_, e) => { e.Cancel = true; cancelSent = true; Engine.Do("ok"); };
                break;
            case "forget":
                dialog.Title = $"Forget {d.S("name")}?";
                dialog.Content = new TextBlock { Text = "It has to be paired again before this PC can connect.", TextWrapping = TextWrapping.Wrap };
                dialog.PrimaryButtonText = "Forget";
                dialog.CloseButtonText = "Cancel";
                dialog.PrimaryButtonClick += (_, e) => { e.Cancel = true; cancelSent = true; Engine.Do("ok"); };
                break;
            case "address":
            {
                string address = d.S("address");
                var box = new TextBox { Header = "Address", Text = address.EndsWith(":7791") ? address[..^5] : address, Width = 360 };
                var col = new StackPanel { Spacing = 12 };
                col.Children.Add(box);
                if (d.S("problem") is { Length: > 0 } problem) col.Children.Add(new InfoBar { IsOpen = true, IsClosable = false, Severity = InfoBarSeverity.Error, Message = problem });
                dialog.Title = "Edit address";
                dialog.Content = col;
                dialog.PrimaryButtonText = "Save";
                dialog.CloseButtonText = "Cancel";
                dialog.DefaultButton = ContentDialogButton.Primary;
                dialog.PrimaryButtonClick += (_, e) => { e.Cancel = true; cancelSent = true; Engine.Do("address-go", new { address = box.Text }); };
                break;
            }
            case "about":
                dialog.Title = "remoteman";
                dialog.Content = new TextBlock { Text = $"Version {d.S("version")}\nSee and use another computer's screen from this one.", TextWrapping = TextWrapping.Wrap };
                dialog.CloseButtonText = "Close";
                break;
            default:
                return;
        }
        // Closed by its own button, by Escape: the engine is told. Hidden because the state
        // moved on: it already knows.
        dialog.Closing += (_, e) =>
        {
            if (e.Result == ContentDialogResult.None && _open == dialog && _kind == kind && !cancelSent) { cancelSent = true; Engine.Do("cancel"); }
        };
        dialog.Closed += (_, _) =>
        {
            if (_open == dialog) _open = null;
            if (_next is { } next) { _next = null; _open = next; _ = next.ShowAsync(); }
        };
        if (_open is null) { _open = dialog; _ = dialog.ShowAsync(); }
        else _next = dialog;
    }
}
