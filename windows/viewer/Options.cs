using Microsoft.UI.Xaml;

namespace Remoteman;

/// What the command line asks for. Ordinary people start remoteman.exe with nothing; the rest is
/// for tests in the VM, where nobody is looking.
///   remoteman.exe [HOST:PORT [--pair]] [--state DIR] [--log FILE] [--dark|--light]
///                 [--seconds N] [--size WxH] [--pos X,Y] [--fullscreen] [--stats]
public sealed class Options
{
    public string? Host;
    public bool Pair;
    public string? State;
    public string? LogFile;
    public ApplicationTheme? Theme;
    public int? Seconds;
    public (int W, int H)? Size;
    public (int X, int Y)? Pos;
    public bool Fullscreen;
    public bool Stats;
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
                case "--pair": o.Pair = true; break;
                case "--state": o.State = Next(); break;
                case "--log": o.LogFile = Next(); break;
                case "--dark": o.Theme = ApplicationTheme.Dark; break;
                case "--light": o.Theme = ApplicationTheme.Light; break;
                case "--seconds": o.Seconds = int.Parse(Next()); break;
                case "--fullscreen": o.Fullscreen = true; break;
                case "--stats": o.Stats = true; o.EngineArgs.Add(a); break;
                case "--size":
                    var p = Next().Split('x');
                    o.Size = (int.Parse(p[0]), int.Parse(p[1]));
                    break;
                case "--pos":
                    var q = Next().Split(',');
                    o.Pos = (int.Parse(q[0]), int.Parse(q[1]));
                    break;
                default:
                    if (a.StartsWith("--")) { o.EngineArgs.Add(a); }
                    else o.Host = a;
                    break;
            }
        }
        return o;
    }
}
