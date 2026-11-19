using System.IO.Pipes;
using System.Text;
using System.Text.Json.Nodes;

namespace RemotemanHost;

/// The running host, as its control pipe says it stands. The pipe takes one line and answers
/// it; the host is asked for `status` once a second and told what the owner chose.
public static class Host
{
    public static int Port = 7791;

    /// Ask the host; `null` if no host answers (it isn't running).
    public static string? Ask(string line)
    {
        // Between two questions the pipe is away for a moment.
        for (int i = 0; i < 10; i++)
        {
            try
            {
                using var pipe = new NamedPipeClientStream(".", $"remoteman-host-{Port}", PipeDirection.InOut);
                pipe.Connect(300);
                var bytes = Encoding.UTF8.GetBytes(line + "\n");
                pipe.Write(bytes);
                pipe.Flush();
                using var reader = new StreamReader(pipe, Encoding.UTF8);
                return reader.ReadToEnd().Trim();
            }
            catch (TimeoutException) { return null; }
            catch (IOException) { Thread.Sleep(30); }
            catch (UnauthorizedAccessException)
            {
                Log.Say("the host's pipe refused this user");
                return null;
            }
        }
        return null;
    }

    public static Status? Status()
    {
        var said = Ask("status");
        if (said is null) return null;
        try { return new Status(JsonNode.Parse(said)!.AsObject()); }
        catch (Exception e)
        {
            Log.Say($"status not understood: {e.Message}");
            return null;
        }
    }

    /// The fingerprint as people compare it: its first 16 characters in fours.
    public static string ShortKey(string fingerprint)
    {
        var s = fingerprint.ToUpperInvariant();
        s = s.Length > 16 ? s[..16] : s;
        return string.Join(" ", Enumerable.Range(0, (s.Length + 3) / 4).Select(i => s.Substring(i * 4, Math.Min(4, s.Length - i * 4))));
    }

    /// A six-digit code in two threes.
    public static string Code(string code)
    {
        var digits = new string(code.Where(char.IsDigit).ToArray());
        return digits.Length == 6 ? $"{digits[..3]} {digits[3..]}" : code;
    }

    /// When something happened, in words: "2 min", "1 h 5 min".
    public static string Ago(long since)
    {
        var span = DateTimeOffset.UtcNow - DateTimeOffset.FromUnixTimeSeconds(since);
        if (span.TotalMinutes < 1) return "just now";
        if (span.TotalHours < 1) return $"{(int)span.TotalMinutes} min";
        return $"{(int)span.TotalHours} h {span.Minutes} min";
    }
}

public record Device(string Name, string Fingerprint, bool Connected);

public record Client(string Name, long Since, int Width, int Height, int Refresh, string Placement, string Picture, bool Sound);

public record Pending(string Name, string Fingerprint, string Code, int Seconds);

/// One answer to `status`.
public sealed class Status
{
    public readonly string Address, Fingerprint, Name;
    public readonly bool Paused, Pairing, Clipboard, Files, Microphone;
    public readonly List<Device> Devices = new();
    public readonly Client? Client;
    public readonly Pending? Pending;
    /// What the device said of files sent to it: how many it answered for, and the last.
    public readonly long Answered;
    public readonly bool SentOk;
    public readonly string Said;

    public Status(JsonObject o)
    {
        string S(JsonNode? n, string k) => n?[k]?.GetValue<string>() ?? "";
        bool B(JsonNode? n, string k) => n?[k] is JsonValue v && v.TryGetValue<bool>(out var b) && b;
        long L(JsonNode? n, string k) => n?[k] is JsonValue v && v.TryGetValue<long>(out var l) ? l : 0;
        Address = S(o, "address");
        Fingerprint = S(o, "fingerprint");
        Name = S(o, "name");
        Paused = B(o, "paused");
        Pairing = B(o, "pairing");
        Clipboard = B(o, "clipboard");
        Files = B(o, "files");
        Microphone = B(o, "microphone");
        if (o["devices"] is JsonArray devices)
            foreach (var d in devices) Devices.Add(new Device(S(d, "name"), S(d, "fingerprint"), B(d, "connected")));
        if (o["client"] is JsonObject c)
            Client = new Client(S(c, "name"), L(c, "since"), (int)L(c, "width"), (int)L(c, "height"), (int)L(c, "refresh"), S(c, "placement"), S(c, "picture"), B(c, "sound"));
        if (o["pending"] is JsonObject p)
            Pending = new Pending(S(p, "name"), S(p, "fingerprint"), S(p, "code"), (int)L(p, "seconds"));
        Answered = L(o["sent"], "answered");
        SentOk = B(o["sent"], "ok");
        Said = S(o["sent"], "said");
    }

    /// What the state is called, and a line or two more: the tray menu's first lines and the
    /// window's status card. As Linux's Remoteman Host says it.
    public (string Title, List<string> Lines) Words(string pc)
    {
        if (Paused) return ($"Not sharing {pc}", new() { "No device can connect until you turn it on. Paired devices stay paired." });
        if (Client is Client c)
        {
            string place = c.Placement switch { "main" => "Main display", "only" => "Only display", "mirror" => "This PC's display", _ => "Extra display" };
            return ("A device is connected", new()
            {
                $"{c.Name} · connected {Host.Ago(c.Since)}",
                $"{c.Width} × {c.Height} at {c.Refresh} Hz · {place}",
                $"{c.Picture}{(c.Sound ? " · sound on" : "")}",
            });
        }
        if (Pairing) return ("Pairing is open", new() { "The next new device to connect is asked about here, then pairing closes." });
        return ("No device is connected", new() { $"Paired devices can connect while {pc} is on." });
    }
}
