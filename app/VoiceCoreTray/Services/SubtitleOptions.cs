using System.Text.Json;

namespace VoiceCoreTray.Services;

/// <summary>
/// Debug / behaviour knobs parsed from the process command line (dialog presenter
/// plus two stack switches), and persistence of the dialog's dragged position.
///
/// Historically the tray took <c>args</c> in <c>OnLaunched</c> but never read
/// them, so the "control subtitle dwell time from the CLI" switch had no code
/// path at all. This type is that missing path.
///
/// Supported flags (all optional, order-independent, tolerant of `=` or space):
///   --subtitle-dwell &lt;seconds&gt;   run an auto-hide countdown of this length
///   --subtitle-test  &lt;text&gt;      pop one sample utterance immediately (stack is NOT started)
///   --subtitle-pin               never auto-hide, whatever the runtime asks for
///   --subtitle-selftest [path]   run the layout matrix, write a text report, exit
///   --no-runtime                 tray + dialog only, never launch the runtime
///
/// SEMANTICS CHANGED with the Hold lifecycle (docs/dialog-presenter.md). The dialog
/// no longer plays and vanishes: when text and audio finish it HOLDS with a blinking
/// indicator. So there is no adaptive dwell left to override -
/// <c>--subtitle-dwell</c> is now how you opt IN to an auto-hide countdown from the
/// command line (the runtime's per-utterance <c>displaySeconds</c> is the other way
/// in), and hovering the dialog freezes that countdown. Everything else kept its
/// meaning; <c>--subtitle-test</c> now holds indefinitely instead of disappearing
/// after a few seconds, and it pre-checks the tray's pin toggle when combined with
/// <c>--subtitle-pin</c>.
/// </summary>
public sealed class SubtitleOptions
{
    /// <summary>Auto-hide countdown length in seconds (--subtitle-dwell).
    /// Null = hold indefinitely unless the runtime pushes <c>displaySeconds</c>.</summary>
    public double? ForcedDwell { get; init; }

    /// <summary>Sample text to show at launch without starting the stack (--subtitle-test).</summary>
    public string? TestText { get; init; }

    /// <summary>Never auto-hide, overriding a runtime-pushed dwell (--subtitle-pin).</summary>
    public bool PinMode { get; init; }

    /// <summary>Report path for the layout self test (--subtitle-selftest), or null.</summary>
    public string? SelfTestReport { get; init; }

    /// <summary>Run the tray without launching the runtime (--no-runtime).</summary>
    public bool NoRuntime { get; init; }

    /// <summary>True when launched only to preview or measure the dialog: the stack
    /// stays down, and so do the tray icon and the global hotkeys.</summary>
    public bool IsStyleProbe => !string.IsNullOrEmpty(TestText) || !string.IsNullOrEmpty(SelfTestReport);

    public static SubtitleOptions Parse(string[]? argv)
    {
        double? dwell = null;
        string? test = null;
        bool pin = false;
        string? selfTest = null;
        bool noRuntime = false;

        argv ??= Array.Empty<string>();
        for (int i = 0; i < argv.Length; i++)
        {
            var (key, inlineValue) = SplitInline(argv[i]);
            switch (key)
            {
                case "--subtitle-dwell":
                    if (TryTakeValue(argv, ref i, inlineValue, out var raw) &&
                        double.TryParse(raw, System.Globalization.NumberStyles.Float,
                            System.Globalization.CultureInfo.InvariantCulture, out var d) && d > 0)
                        dwell = d;
                    break;
                case "--subtitle-test":
                    if (TryTakeValue(argv, ref i, inlineValue, out var txt))
                        test = txt;
                    break;
                case "--subtitle-pin":
                    pin = true;
                    break;
                case "--subtitle-selftest":
                    selfTest = TryTakeValue(argv, ref i, inlineValue, out var path) && path.Length > 0
                        ? path
                        : Path.Combine(Path.GetTempPath(), "vc-subtitle-selftest.txt");
                    break;
                case "--no-runtime":
                    noRuntime = true;
                    break;
            }
        }

        return new SubtitleOptions
        {
            ForcedDwell = dwell,
            TestText = test,
            PinMode = pin,
            SelfTestReport = selfTest,
            NoRuntime = noRuntime,
        };
    }

    private static (string key, string? inline) SplitInline(string arg)
    {
        var eq = arg.IndexOf('=');
        return eq < 0 ? (arg, null) : (arg[..eq], arg[(eq + 1)..]);
    }

    /// <summary>Take a value from <c>--flag=value</c> or the following token.</summary>
    private static bool TryTakeValue(string[] argv, ref int i, string? inline, out string value)
    {
        if (inline is not null) { value = inline; return true; }
        if (i + 1 < argv.Length && !argv[i + 1].StartsWith("--"))
        {
            value = argv[++i];
            return true;
        }
        value = "";
        return false;
    }

    // -- dragged-position persistence (unpackaged app: PersistenceId is unavailable) --
    //
    // The stored point is the dialog box's BOTTOM-CENTER in screen physical pixels,
    // not the window origin: the dialog window is a fixed oversized canvas, so a
    // window origin would mean something different for every line count. The file
    // name and format are unchanged, so a position dragged by an older build still
    // applies.

    private sealed record Pos(int X, int Y);

    private static string PosFile(string dataDir) => Path.Combine(dataDir, "subtitle-pos.json");

    /// <summary>Load the capsule anchor (bottom-center, physical pixels), or null.</summary>
    public static (int x, int y)? LoadAnchor(string dataDir)
    {
        try
        {
            var file = PosFile(dataDir);
            if (!File.Exists(file)) return null;
            var pos = JsonSerializer.Deserialize<Pos>(File.ReadAllText(file));
            return pos is null ? null : (pos.X, pos.Y);
        }
        catch { return null; }
    }

    /// <summary>Persist the capsule anchor (bottom-center, physical pixels). Best-effort.</summary>
    public static void SaveAnchor(string dataDir, int x, int y)
    {
        try
        {
            Directory.CreateDirectory(dataDir);
            File.WriteAllText(PosFile(dataDir), JsonSerializer.Serialize(new Pos(x, y)));
        }
        catch { /* non-fatal */ }
    }

    /// <summary>Forget the anchor so the overlay snaps back to bottom-center.</summary>
    public static void ClearAnchor(string dataDir)
    {
        try
        {
            var file = PosFile(dataDir);
            if (File.Exists(file)) File.Delete(file);
        }
        catch { /* non-fatal */ }
    }
}
