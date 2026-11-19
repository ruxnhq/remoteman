using System.Text.Json.Nodes;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Remoteman;

/// Which page the engine's state calls for, and keeping each up to date.
public sealed class Views
{
    readonly MainWindow _w;
    readonly ComputersPage _computers = new();
    readonly StatusPage _status = new();
    readonly SessionBar _bar = new();
    string _kind = "";

    public Views(MainWindow w) => _w = w;

    /// Once the window is up.
    public void Start() { }

    public void Show(JsonObject state)
    {
        var screen = state.O("screen");
        string kind = screen.S("kind");
        if (kind != _kind) Log.Say($"screen: {kind}");
        _kind = kind;
        _w.SetBack(kind is "settings" or "computer" or "problem");
        switch (kind)
        {
            case "session":
                _bar.Update(state);
                _w.SetBar(_bar);
                _w.SetBody(null);
                break;
            case "connecting":
            case "problem":
                _w.SetBar(null);
                _status.Update(state);
                _w.SetBody(_status);
                break;
            default:
                _w.SetBar(null);
                _computers.Update(state);
                _w.SetBody(_computers);
                break;
        }
        Dialogs.Update(_w, state);
    }
}

/// The list of computers.
public sealed class ComputersPage : UserControl
{
    readonly StackPanel _list = new() { Spacing = 8 };
    readonly TextBlock _empty = new() { Text = "No computers yet. Pair one to see it here.", Foreground = Theme.Secondary };
    string _shown = "";

    public ComputersPage()
    {
        var pair = new Button { Content = Ui.Row(Ui.Icon(""), new TextBlock { Text = "Pair a computer" }), Style = Ui.Accent };
        pair.Click += (_, _) => Engine.Do("pair");
        var settings = new Button { Content = Ui.Icon(""), Width = 32, Height = 32, Padding = new Thickness(0) };
        ToolTipService.SetToolTip(settings, "Settings");
        settings.Click += (_, _) => Engine.Do("settings");
        var head = new Grid { ColumnSpacing = 8 };
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.Children.Add(new TextBlock { Text = "Computers", Style = Ui.Style("TitleTextBlockStyle") });
        Grid.SetColumn(pair, 1); head.Children.Add(pair);
        Grid.SetColumn(settings, 2); head.Children.Add(settings);
        var body = new StackPanel { Spacing = 20, Padding = new Thickness(32, 12, 32, 28) };
        body.Children.Add(head);
        body.Children.Add(new TextBlock { Text = "Paired computers", Style = Ui.Style("BodyStrongTextBlockStyle") });
        body.Children.Add(_empty);
        body.Children.Add(_list);
        Content = new ScrollViewer { Content = body };
    }

    public void Update(JsonObject state)
    {
        var list = state.A("computers");
        string key = list.ToJsonString();
        if (key == _shown) return;
        _shown = key;
        _list.Children.Clear();
        _empty.Visibility = list.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        foreach (var c in list) _list.Children.Add(Card(c!));
    }

    static UIElement Card(JsonNode c)
    {
        string address = c.S("address");
        var connect = new Button { Content = "Connect", Style = Ui.Accent };
        connect.Click += (_, _) => Engine.Do("connect", new { address });
        var more = new Button { Content = Ui.Icon(""), Width = 32, Height = 32, Padding = new Thickness(0) };
        ToolTipService.SetToolTip(more, "More");
        var menu = new MenuFlyout();
        menu.Items.Add(Ui.Item("Settings for this computer…", () => Engine.Do("computer", new { address })));
        menu.Items.Add(Ui.Item("Forget…", () => Engine.Do("forget", new { address })));
        more.Flyout = menu;
        string line = string.Join(" · ", new[] { c.S("os_name"), c.S("short"), c.S("last") is { Length: > 0 } l ? $"Last used {l}" : "" }.Where(x => x.Length > 0));
        var words = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        words.Children.Add(new TextBlock { Text = c.S("name"), Style = Ui.Style("BodyStrongTextBlockStyle") });
        words.Children.Add(new TextBlock { Text = line, Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary });
        var row = new Grid { ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.Children.Add(words);
        Grid.SetColumn(connect, 1); row.Children.Add(connect);
        Grid.SetColumn(more, 2); row.Children.Add(more);
        return Ui.Card(row);
    }
}

/// Connecting, and the pages that say why there is no session.
public sealed class StatusPage : UserControl
{
    readonly ProgressRing _ring = new() { Width = 40, Height = 40, IsActive = true };
    readonly TextBlock _title = new() { Style = Ui.Style("SubtitleTextBlockStyle"), TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap };
    readonly TextBlock _words = new() { TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap, Foreground = Theme.Secondary, MaxWidth = 460 };
    readonly StackPanel _acts = new() { Orientation = Orientation.Horizontal, Spacing = 8, HorizontalAlignment = HorizontalAlignment.Center };
    readonly PasswordBox _password = new() { Width = 300, PlaceholderText = "Password" };
    readonly CheckBox _remember = new() { Content = "Remember it on this PC" };
    readonly StackPanel _locked = new() { Spacing = 8, HorizontalAlignment = HorizontalAlignment.Center };
    string _shown = "";

    public StatusPage()
    {
        _locked.Children.Add(_password);
        _locked.Children.Add(_remember);
        _password.KeyDown += (_, e) => { if (e.Key == Windows.System.VirtualKey.Enter) Unlock(); };
        var col = new StackPanel { Spacing = 16, VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Center, MaxWidth = 520 };
        col.Children.Add(_ring);
        col.Children.Add(_title);
        col.Children.Add(_words);
        col.Children.Add(_locked);
        col.Children.Add(_acts);
        Content = col;
    }

    void Unlock() => Engine.Do("unlock", new { password = _password.Password, remember = _remember.IsChecked == true });

    public void Update(JsonObject state)
    {
        var s = state.O("screen");
        string key = s!.ToJsonString();
        if (key == _shown) return;
        _shown = key;
        string name = s.S("name") is { Length: > 0 } n ? n : s.S("address");
        string address = s.S("address");
        _acts.Children.Clear();
        _locked.Visibility = Visibility.Collapsed;
        _ring.Visibility = Visibility.Collapsed;
        void Act(string text, Action act, bool accent = false)
        {
            var b = new Button { Content = text };
            if (accent) b.Style = Ui.Accent;
            b.Click += (_, _) => act();
            _acts.Children.Add(b);
        }
        if (s.S("kind") == "connecting")
        {
            _ring.Visibility = Visibility.Visible;
            _title.Text = $"Connecting to {name}";
            _words.Text = "Finding the computer, checking it is the one this PC paired with, and starting the picture.";
            Act("Cancel", () => Engine.Do("cancel-connect"));
            return;
        }
        var p = s.O("problem");
        string said = p.S("said");
        switch (p.S("kind"))
        {
            case "unreachable":
                _title.Text = $"{name} isn't answering";
                _words.Text = $"{said}. Check that it is on, awake and on this network, and that remoteman is running on it.";
                Act("Try again", () => Engine.Do("connect", new { address }), true);
                Act("Back to computers", () => Engine.Do("back"));
                break;
            case "asleep":
                _title.Text = $"{name} is asleep";
                _words.Text = "Waking it over the network isn't available yet.";
                Act("Back to computers", () => Engine.Do("back"));
                break;
            case "not-paired":
                _title.Text = $"{name} doesn't know this PC";
                _words.Text = "It no longer trusts this PC, or it was set up again. Pair them again to connect.";
                Act("Pair again", () => Engine.Do("pair", new { address }), true);
                Act("Back to computers", () => Engine.Do("back"));
                break;
            case "changed":
                _title.Text = $"This isn't the {name} this PC paired with";
                _words.Text = "Something else answered at that address with another key. If the computer was set up again, forget it and pair again.";
                Act("Forget it", () => Engine.Do("forget", new { address }));
                Act("Back to computers", () => Engine.Do("back"), true);
                break;
            case "refused":
                _title.Text = $"{name} said no";
                _words.Text = said;
                Act("Try again", () => Engine.Do("connect", new { address }), true);
                Act("Back to computers", () => Engine.Do("back"));
                break;
            case "logging-in":
                _ring.Visibility = Visibility.Visible;
                _title.Text = $"{name} is logging in";
                _words.Text = said.Length > 0 ? $"{said}. This PC tries again by itself." : "This PC tries again by itself.";
                Act("Try now", () => Engine.Do("connect", new { address }), true);
                Act("Back to computers", () => Engine.Do("back"));
                break;
            case "locked":
                _title.Text = $"{name} is locked";
                _words.Text = said;
                _locked.Visibility = Visibility.Visible;
                _password.Password = "";
                _remember.IsChecked = state.B("remember");
                Act("Unlock", Unlock, true);
                Act("Back to computers", () => Engine.Do("back"));
                _password.Focus(FocusState.Programmatic);
                break;
            default:
                _title.Text = $"The session with {name} ended";
                _words.Text = said;
                Act("Connect again", () => Engine.Do("connect", new { address }), true);
                Act("Back to computers", () => Engine.Do("back"));
                break;
        }
    }
}

/// The session's bar, in the title bar.
public sealed class SessionBar : UserControl
{
    readonly TextBlock _name = new() { Style = Ui.Style("BodyStrongTextBlockStyle"), VerticalAlignment = VerticalAlignment.Center };
    readonly TextBlock _format = new() { Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary, VerticalAlignment = VerticalAlignment.Center };
    readonly TextBlock _figures = new() { Style = Ui.Style("CaptionTextBlockStyle"), Foreground = Theme.Secondary, VerticalAlignment = VerticalAlignment.Center };

    public SessionBar()
    {
        var full = new Button { Content = Ui.Icon(""), Width = 32, Height = 32, Padding = new Thickness(0) };
        ToolTipService.SetToolTip(full, "Full screen (F11)");
        full.Click += (_, _) => Engine.Do("fullscreen");
        var off = new Button { Content = Ui.Row(Ui.Icon(""), new TextBlock { Text = "Disconnect" }) };
        off.Click += (_, _) => Engine.Do("disconnect");
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10, VerticalAlignment = VerticalAlignment.Center };
        row.Children.Add(_name);
        row.Children.Add(_format);
        row.Children.Add(_figures);
        row.Children.Add(full);
        row.Children.Add(off);
        Content = row;
    }

    public void Update(JsonObject state)
    {
        var s = state.O("session");
        _name.Text = s.S("name");
        var f = s?["format"] as JsonArray;
        _format.Text = f is { Count: 3 } ? $"{f[0]}×{f[1]} · {f[2]} Hz" : "";
        var fig = s.O("figures");
        _figures.Text = state.O("settings").B("figures") && fig is not null ? $"{fig.I("shown")} shown · {fig.I("decoded")} decoded · {fig.D("age_ms"):0.0} ms · {fig.D("megabits"):0.0} Mb/s · {fig.S("decoder")}" : "";
    }
}

/// Small helpers for building pages in code.
static class Ui
{
    public static Style Style(string name) => (Style)Application.Current.Resources[name];
    public static Style Accent => Style("AccentButtonStyle");
    public static FontIcon Icon(string glyph) => new() { Glyph = glyph, FontSize = 16 };
    public static StackPanel Row(params UIElement[] items)
    {
        var p = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        foreach (var i in items) p.Children.Add(i);
        return p;
    }
    public static MenuFlyoutItem Item(string text, Action act)
    {
        var i = new MenuFlyoutItem { Text = text };
        i.Click += (_, _) => act();
        return i;
    }
    public static Border Card(UIElement child) => new()
    {
        Child = child,
        Padding = new Thickness(16, 10, 16, 10),
        MinHeight = 60,
        CornerRadius = new CornerRadius(6),
        BorderThickness = new Thickness(1),
        Background = (Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"],
        BorderBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
    };
}

static class Theme
{
    public static Brush Secondary => (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
}

/// The dialogs the state calls for, as ContentDialogs.
static class Dialogs
{
    static ContentDialog? _open, _next;
    static string _kind = "";

    public static void Update(MainWindow w, JsonObject state)
    {
        var d = state.O("dialog");
        string kind = d is null ? "" : d.S("kind") + d.S("waiting") + d.S("problem");
        if (kind == _kind) return;
        _kind = kind;
        _next = null;
        if (_open is not null) _open.Hide();
        if (d is null) return;
        var dialog = new ContentDialog { XamlRoot = w.Content.XamlRoot };
        switch (d.S("kind"))
        {
            case "pair":
                var box = new TextBox { Header = "Address", PlaceholderText = "name.local or 192.168.1.20" };
                var col = new StackPanel { Spacing = 12 };
                col.Children.Add(new TextBlock { Text = "Start pairing on the other computer first, then type its name or address here.", TextWrapping = TextWrapping.Wrap });
                col.Children.Add(box);
                if (d.S("waiting") is { Length: > 0 } waiting) { col.Children.Add(Ui.Row(new ProgressRing { Width = 16, Height = 16, IsActive = true }, new TextBlock { Text = $"Waiting for {waiting}…" })); box.IsEnabled = false; dialog.IsPrimaryButtonEnabled = false; }
                if (d.S("problem") is { Length: > 0 } problem) col.Children.Add(new InfoBar { IsOpen = true, IsClosable = false, Severity = InfoBarSeverity.Error, Message = problem });
                dialog.Title = "Pair a computer";
                dialog.Content = col;
                dialog.PrimaryButtonText = "Pair";
                dialog.CloseButtonText = "Cancel";
                dialog.DefaultButton = ContentDialogButton.Primary;
                dialog.PrimaryButtonClick += (_, e) => { e.Cancel = true; Engine.Do("pair-go", new { address = box.Text }); };
                break;
            case "code":
                dialog.Title = "Confirm the code";
                dialog.Content = new TextBlock { Text = $"{d.S("name")} is asking its owner about this PC. Check that it shows this code:\n\n{d.S("code")}", TextWrapping = TextWrapping.Wrap };
                dialog.CloseButtonText = "Cancel";
                break;
            case "paired":
                dialog.Title = $"Paired with {d.S("name")}";
                dialog.Content = new TextBlock { Text = $"Its key: {d.S("key")}", TextWrapping = TextWrapping.Wrap };
                dialog.PrimaryButtonText = "Connect";
                dialog.CloseButtonText = "Not now";
                dialog.DefaultButton = ContentDialogButton.Primary;
                dialog.PrimaryButtonClick += (_, e) => { e.Cancel = true; Engine.Do("ok"); };
                break;
            case "forget":
                dialog.Title = $"Forget {d.S("name")}?";
                dialog.Content = new TextBlock { Text = "This PC will need to be paired with it again to connect.", TextWrapping = TextWrapping.Wrap };
                dialog.PrimaryButtonText = "Forget";
                dialog.CloseButtonText = "Cancel";
                dialog.PrimaryButtonClick += (_, e) => { e.Cancel = true; Engine.Do("ok"); };
                break;
            case "about":
                dialog.Title = "remoteman";
                dialog.Content = new TextBlock { Text = $"Version {d.S("version")}" };
                dialog.CloseButtonText = "Close";
                break;
            default:
                return;
        }
        dialog.CloseButtonClick += (_, _) => Engine.Do("cancel");
        dialog.Closed += (_, _) =>
        {
            if (_open == dialog) _open = null;
            // One dialog at a time: the next one waits for this one to have gone.
            if (_next is { } next) { _next = null; _open = next; _ = next.ShowAsync(); }
        };
        if (_open is null) { _open = dialog; _ = dialog.ShowAsync(); }
        else _next = dialog;
    }
}
