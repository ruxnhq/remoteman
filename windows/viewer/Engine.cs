using System.Diagnostics.CodeAnalysis;
using System.Runtime.InteropServices;
using System.Text.Json.Nodes;

namespace Remoteman;

/// remoteman_engine.dll, the Rust engine behind this app (crates/remoteman-viewer-windows,
/// src/engine.rs has the other side). Every call is made on the window's thread.
public static unsafe partial class Engine
{
    const string Dll = "remoteman_engine.dll";

    [LibraryImport(Dll)] private static partial int remoteman_start([MarshalAs(UnmanagedType.LPWStr)] string args, delegate* unmanaged[Stdcall]<void> notify);
    [LibraryImport(Dll)] private static partial nint remoteman_error();
    [LibraryImport(Dll)] private static partial nint remoteman_state();
    [LibraryImport(Dll)] private static partial int remoteman_command([MarshalAs(UnmanagedType.LPWStr)] string json);
    [LibraryImport(Dll)] private static partial int remoteman_attach(nint panel, nint window);
    [LibraryImport(Dll)] public static partial void remoteman_panel(int x, int y, int width, int height, float scaleX, float scaleY);
    [LibraryImport(Dll)] public static partial void remoteman_pointer(int x, int y);
    [LibraryImport(Dll)] public static partial void remoteman_button(int button, [MarshalAs(UnmanagedType.U1)] bool down, int x, int y);
    [LibraryImport(Dll)] public static partial void remoteman_wheel(float notches, [MarshalAs(UnmanagedType.U1)] bool horizontal);
    [LibraryImport(Dll)] [return: MarshalAs(UnmanagedType.U1)] public static partial bool remoteman_key(uint scancode, [MarshalAs(UnmanagedType.U1)] bool extended, [MarshalAs(UnmanagedType.U1)] bool down, [MarshalAs(UnmanagedType.U1)] bool repeat);
    [LibraryImport(Dll)] public static partial void remoteman_focus([MarshalAs(UnmanagedType.U1)] bool focused);
    // phase: 0 down, 1 move, 2 up (or cancelled, or capture lost); true when it went as a touch.
    [LibraryImport(Dll)] [return: MarshalAs(UnmanagedType.U1)] public static partial bool remoteman_touch(uint id, int phase, int x, int y);
    [LibraryImport(Dll)] public static partial nint remoteman_cursor(out uint serial);
    [LibraryImport(Dll)] private static partial int remoteman_stop();

    /// Called (on the window's thread) when the state has changed.
    public static event Action? Changed;
    static bool _queued;

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(System.Runtime.CompilerServices.CallConvStdcall) })]
    static void Notify()
    {
        // Not now: this may be in the middle of a call into the engine.
        if (_queued) return;
        _queued = true;
        App.Window?.DispatcherQueue.TryEnqueue(() => { _queued = false; Changed?.Invoke(); });
    }

    public static string? Start(IEnumerable<string> args)
    {
        if (remoteman_start(string.Join("\n", args), &Notify) == 0) return null;
        return Marshal.PtrToStringUni(remoteman_error()) ?? "the engine didn't start";
    }

    public static JsonObject State()
    {
        nint p = remoteman_state();
        return p == 0 ? new JsonObject() : JsonNode.Parse(Marshal.PtrToStringUni(p)!)!.AsObject();
    }

    /// `{"do": WHAT, ...}`: what people did, for the engine to decide.
    public static bool Do(string what) => Do<object>(what, null);

    // Generic, with the properties kept, so that trimming .NET (the MSIX build) keeps the
    // properties of the anonymous objects passed here.
    public static bool Do<[DynamicallyAccessedMembers(DynamicallyAccessedMemberTypes.PublicProperties)] T>(string what, T? with) where T : class
    {
        var o = with is null ? new JsonObject() : JsonSerializerHelper.ToObject(with);
        o["do"] = what;
        bool ok = remoteman_command(o.ToJsonString()) == 0;
        if (!ok) Log.Say($"the engine didn't take {o.ToJsonString()}");
        return ok;
    }

    public static string? Attach(object panel, nint window)
    {
        nint unknown = ((WinRT.IWinRTObject)panel).NativeObject.ThisPtr;
        if (remoteman_attach(unknown, window) == 0) return null;
        return Marshal.PtrToStringUni(remoteman_error());
    }

    public static int Stop() => remoteman_stop();
}

static class JsonSerializerHelper
{
    public static JsonObject ToObject<[DynamicallyAccessedMembers(DynamicallyAccessedMemberTypes.PublicProperties)] T>(T o) where T : class
    {
        var result = new JsonObject();
        foreach (var p in typeof(T).GetProperties())
        {
            var v = p.GetValue(o);
            result[p.Name] = v switch
            {
                null => null,
                bool b => JsonValue.Create(b),
                int i => JsonValue.Create(i),
                long l => JsonValue.Create(l),
                double d => JsonValue.Create(d),
                _ => JsonValue.Create(v.ToString()),
            };
        }
        return result;
    }
}

/// Reading the state.
static class J
{
    public static string S(this JsonNode? n, string key) => n?[key]?.GetValueKind() == System.Text.Json.JsonValueKind.String ? n[key]!.GetValue<string>() : "";
    public static bool B(this JsonNode? n, string key) => n?[key]?.GetValueKind() == System.Text.Json.JsonValueKind.True;
    public static double D(this JsonNode? n, string key) => n?[key]?.GetValueKind() == System.Text.Json.JsonValueKind.Number ? n[key]!.GetValue<double>() : 0;
    public static int I(this JsonNode? n, string key) => (int)n.D(key);
    public static JsonArray A(this JsonNode? n, string key) => n?[key] as JsonArray ?? new JsonArray();
    public static JsonObject? O(this JsonNode? n, string key) => n?[key] as JsonObject;
}
