using Microsoft.UI.Xaml;

namespace Remoteman;

/// The command line. Ordinary people start remoteman.exe with nothing; the rest is for tests in
/// the VM. The engine takes the old program's words (HOST:PORT, --pair, --state, --log,
/// --seconds, --stats, --size for the display asked for, ...); these are the view's own:
///   --window WxH   the window's size, in the app's units
///   --pos X,Y      where the window goes, in pixels
///   --dark|--light the theme (the engine is told too)
public sealed class Options
{
    public string? LogFile;
    public ApplicationTheme? Theme;
    public (int W, int H)? Window;
    public (int X, int Y)? Pos;
    public List<string> EngineArgs = new();

    public static Options Parse(string[] args)
    {
        var o = new Options();
        for (int i = 0; i < args.Length; i++)
        {
            string a = args[i];
            string Next() => i + 1 < args.Length ? args[++i] : throw new ArgumentException($"{a} needs a value");
            switch (a)
            {
                case "--window":
                    var p = Next().Split('x');
                    o.Window = (int.Parse(p[0]), int.Parse(p[1]));
                    break;
                case "--pos":
                    var q = Next().Split(',');
                    o.Pos = (int.Parse(q[0]), int.Parse(q[1]));
                    break;
                case "--log":
                    o.LogFile = Next();
                    // The engine writes the file; the view's lines go beside it.
                    o.EngineArgs.Add(a); o.EngineArgs.Add(o.LogFile);
                    o.LogFile += ".view";
                    break;
                case "--dark": o.Theme = ApplicationTheme.Dark; o.EngineArgs.Add(a); break;
                case "--light": o.Theme = ApplicationTheme.Light; o.EngineArgs.Add(a); break;
                default: o.EngineArgs.Add(a); break;
            }
        }
        return o;
    }
}
