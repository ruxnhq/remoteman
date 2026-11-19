namespace RemotemanHost;

/// The app's own lines, to the file given with --log.
public static class Log
{
    static readonly object Gate = new();

    public static void Say(string line)
    {
        string text = $"{DateTime.Now:HH:mm:ss.fff} app: {line}";
        System.Diagnostics.Debug.WriteLine(text);
        if (App.LogFile is string path)
            lock (Gate) File.AppendAllText(path, text + Environment.NewLine);
    }
}
