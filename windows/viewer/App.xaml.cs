using Microsoft.UI.Xaml;

namespace Remoteman;

public partial class App : Application
{
    public static MainWindow? Window { get; private set; }
    public static Options Options { get; private set; } = new();

    public App()
    {
        Options = Options.Parse(Environment.GetCommandLineArgs().Skip(1).ToArray());
        // Packaged (the Store's MSIX): the state lives in the package's own data folder, which
        // Windows removes with the app, rather than in %APPDATA%.
        if (Packaging.IsPackaged && !Options.EngineArgs.Contains("--state"))
        {
            Options.EngineArgs.Add("--state");
            Options.EngineArgs.Add(Path.Combine(Windows.Storage.ApplicationData.Current.LocalFolder.Path, "remoteman"));
        }
        // The theme has to be chosen before any window exists.
        if (Options.Theme is ApplicationTheme theme) RequestedTheme = theme;
        InitializeComponent();
        UnhandledException += (_, e) =>
        {
            Log.Say($"unhandled: {e.Exception}");
            // Without --log (as the Start menu starts it), still leave the reason somewhere.
            if (Options.LogFile is null)
                try
                {
                    string dir = Packaging.IsPackaged ? Windows.Storage.ApplicationData.Current.LocalFolder.Path : Path.GetTempPath();
                    File.AppendAllText(Path.Combine(dir, "remoteman-crash.log"), $"{DateTime.Now:s} {e.Exception}{Environment.NewLine}");
                }
                catch { }
        };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        Window = new MainWindow();
        Window.Activate();
    }
}
