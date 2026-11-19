namespace Remoteman;

/// The view's own lines, in the same file as the engine's when --log is given.
public static class Log
{
    static readonly object Gate = new();

    public static void Say(string line)
    {
        string text = $"{DateTime.Now:HH:mm:ss.fff} view: {line}";
        System.Diagnostics.Debug.WriteLine(text);
        if (App.Options.LogFile is string path)
            lock (Gate) File.AppendAllText(path, text + Environment.NewLine);
    }
}
