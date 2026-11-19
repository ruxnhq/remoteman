using Microsoft.UI.Xaml;

namespace Remoteman;

public partial class App : Application
{
    public static MainWindow? Window { get; private set; }
    public static Options Options { get; private set; } = new();

    public App()
    {
        Options = Options.Parse(Environment.GetCommandLineArgs().Skip(1).ToArray());
        // The theme has to be chosen before any window exists.
        if (Options.Theme is ApplicationTheme theme) RequestedTheme = theme;
        InitializeComponent();
        UnhandledException += (_, e) => Log.Say($"unhandled: {e.Exception}");
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        Window = new MainWindow();
        Window.Activate();
    }
}
