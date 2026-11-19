using Microsoft.UI.Xaml;

namespace RemotemanHost;

/// Remoteman Host: `remoteman-host-app [--background] [--page sharing|devices|about]
/// [--port PORT] [--log FILE] [--theme light|dark]`. `--background` starts with the icon only
/// (as at sign-in); otherwise the window opens too. One instance a user: a second one only
/// opens the first's window.
public partial class App : Application
{
    public static string? LogFile;
    public static bool Background;
    public static string Page = "sharing";
    public static MainWindow? Window;

    public App()
    {
        var args = Environment.GetCommandLineArgs().Skip(1).ToArray();
        for (int i = 0; i < args.Length; i++)
        {
            string Next() => i + 1 < args.Length ? args[++i] : "";
            switch (args[i])
            {
                case "--log": LogFile = Next(); break;
                case "--background": Background = true; break;
                case "--page": Page = Next(); break;
                case "--port": Host.Port = int.TryParse(Next(), out var p) ? p : 7791; break;
                case "--theme": RequestedTheme = Next() == "dark" ? ApplicationTheme.Dark : ApplicationTheme.Light; break;
            }
        }
        InitializeComponent();
        UnhandledException += (_, e) => Log.Say($"unhandled: {e.Message} {e.Exception}");
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        Window = new MainWindow();
        if (!Background) Window.ShowWindow(Page);
    }
}
