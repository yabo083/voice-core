using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace VoiceCoreTray.Dialog;

/// <summary>How the text of an utterance arrives on screen.</summary>
internal enum RevealStyle
{
    /// <summary>Character by character, paced against the audio. The Galgame default.</summary>
    Typewriter,
    /// <summary>A feathered light sweep travels left to right and wipes the finished layout
    /// in. Zero layout movement: everything is measured and placed before the animation
    /// starts, and only an opacity mask moves.</summary>
    Sweep,
    /// <summary>Segment by segment opacity fade, staggered. Print-like: no wipe edge, no
    /// trailing, the cleanest of the three.</summary>
    Fade,
}

/// <summary>
/// Everything about this app that a user may change, in ONE file: <c>config.json</c> in
/// the runtime's data dir. Read once at startup, written with the defaults when absent,
/// and edited from the tray's single 设置 entry - a change needs a tray restart.
///
/// It replaces the earlier per-feature files (<c>dialog.json</c>, <c>hotkeys.json</c>,
/// <c>voicepacks.json</c>): a setting nobody can find is a setting nobody uses, and four
/// files to find one in are three too many. Values from those files are carried over
/// once, then the files are removed. <c>runtime.json</c> stays separate because the
/// runtime owns it, and <c>subtitle-pos.json</c> because it is state the dialog writes,
/// not a preference anyone edits.
///
/// The tray is the only writer. The Rust runtime READS the <c>voicePacks</c> section from
/// the same file (see <c>src/packs.rs</c>), reloading it whenever the mtime changes, so
/// that section is carried verbatim through every write here - this class must never be
/// the reason a pack disappears.
/// </summary>
internal sealed record AppConfig
{
    public const string FileName = "config.json";

    [JsonPropertyName("dialog")]
    public DialogSection Dialog { get; init; } = new();

    [JsonPropertyName("hotkeys")]
    public HotkeySection Hotkeys { get; init; } = new();

    /// <summary>
    /// The voice pack registry, owned by the runtime and passed through here untouched.
    /// Kept as raw JSON rather than typed: the tray neither reads nor validates a pack,
    /// and a typed mirror of the runtime's <c>VoicePack</c> would be a second definition
    /// to keep in step for no gain - and would quietly drop any field it did not know.
    /// </summary>
    [JsonPropertyName("voicePacks")]
    public JsonElement? VoicePacks { get; init; }

    internal sealed record DialogSection
    {
        /// <summary>
        /// Where the spoken line goes relative to the line being read: above it, or below.
        ///
        /// It is an ANNOTATION, not a second subtitle - the Galgame descendant of ruby text
        /// (振り仮名), which normally carries a reading over a kanji and got repurposed for a
        /// small aside under (or over) the spoken line. So it hugs the base line rather than
        /// sitting in its own row, and which side it hugs is taste.
        /// </summary>
        [JsonPropertyName("annotationAbove")]
        public bool AnnotationAbove { get; init; }

        /// <summary>`typewriter`, `sweep` or `fade`.</summary>
        [JsonPropertyName("reveal")]
        public string Reveal { get; init; } = "typewriter";

        public RevealStyle Style => Reveal?.Trim().ToLowerInvariant() switch
        {
            "sweep" => RevealStyle.Sweep,
            "fade" => RevealStyle.Fade,
            _ => RevealStyle.Typewriter,
        };
    }

    internal sealed record HotkeySection
    {
        [JsonPropertyName("toggleDialog")]
        public string ToggleDialog { get; init; } = "Ctrl+Alt+D";

        [JsonPropertyName("toggleHold")]
        public string ToggleHold { get; init; } = "Ctrl+Alt+H";
    }

    /// <summary>Reader options: this file is edited by hand, so comments and a trailing
    /// comma are accepted rather than punished.</summary>
    private static readonly JsonSerializerOptions ReadOptions = new()
    {
        ReadCommentHandling = JsonCommentHandling.Skip,
        AllowTrailingCommas = true,
    };

    public static AppConfig Load(string dataDir, Action<string>? note = null)
    {
        var file = Path.Combine(dataDir, FileName);
        AppConfig config;
        try
        {
            config = File.Exists(file)
                ? JsonSerializer.Deserialize<AppConfig>(File.ReadAllText(file), ReadOptions) ?? new AppConfig()
                : new AppConfig();
        }
        catch (Exception ex)
        {
            // A malformed preferences file must not stop the app from starting; the defaults
            // are the whole fallback, and the user gets told rather than guessing.
            note?.Invoke($"读取 {file} 失败，使用默认设置：{ex.Message}");
            return new AppConfig();
        }

        // A section this file has never carried can still exist as a legacy file next to it -
        // a data dir written by an older build, or one upgraded in place. Fold it in on
        // sight rather than only on a fresh install, or the packs would look deleted.
        var (merged, consumed) = Merge(dataDir, config);
        if (consumed.Count == 0 && File.Exists(file)) return merged;

        try
        {
            Save(dataDir, merged);
        }
        catch (Exception ex)
        {
            // The legacy files are still there, so nothing is lost - but say so plainly:
            // this is a WRITE failure, and the previous wording blamed the read.
            note?.Invoke($"写入 {file} 失败，本次使用内存中的设置：{ex.Message}");
            return merged;
        }

        // Only now: the merged file is on disk, so removing the sources cannot lose them.
        foreach (var name in consumed)
        {
            try { File.Delete(Path.Combine(dataDir, name)); }
            catch { /* a leftover file is cosmetic; the merge is already durable */ }
        }
        if (consumed.Count > 0) note?.Invoke($"设置已合并到 {FileName}（原 {string.Join("、", consumed)} 已移除）。");
        return merged;
    }

    /// <summary>
    /// Written as an annotated template rather than serializer output: this file's whole job
    /// is to be opened in notepad by a human, and a bare <c>"reveal": "typewriter"</c> does
    /// not tell them what else they may type there. Comments are read back (see
    /// <see cref="ReadOptions"/>), so the annotations survive editing.
    ///
    /// Written to a sibling and renamed over the target, NOT in place: the runtime reads
    /// this same file whenever its mtime moves, and a truncating in-place write is
    /// observable as zero bytes or a prefix. <c>File.Move(overwrite: true)</c> is
    /// <c>MoveFileEx(MOVEFILE_REPLACE_EXISTING)</c>, so a reader sees either the whole old
    /// file or the whole new one, and the mtime moves exactly once.
    ///
    /// Every interpolated scalar goes through the serializer. Two of them are typed by hand
    /// (a hotkey spec, the reveal name) and one quote or backslash would otherwise emit a
    /// broken file - taking the <c>voicePacks</c> section down with it, which is the one
    /// thing this class must never do.
    /// </summary>
    public static void Save(string dataDir, AppConfig config)
    {
        Directory.CreateDirectory(dataDir);
        var target = Path.Combine(dataDir, FileName);
        var temp = target + ".new";
        File.WriteAllText(temp, $$"""
        // voice-core 设置。改完重启托盘生效（声线包除外，运行时会自己重新读取）。
        // 注释和尾随逗号都可以保留，不会导致解析失败。
        {
          "dialog": {
            // 旁注（上游给的原文/译文）在正读的那行上方还是下方：true 在上，false 在下。
            "annotationAbove": {{(config.Dialog.AnnotationAbove ? "true" : "false")}},

            // 文字出现方式：
            //   "typewriter" 逐字打字机，按音频时长配速（默认，Galgame 手感）
            //   "sweep"      一道柔光从左到右扫过，把已排好的整行抹出（零排版抖动）
            //   "fade"       整段按子句依次淡入，印刷质感，最干净
            "reveal": {{Scalar(config.Dialog.Reveal)}}
          },

          // 全局快捷键。至少要带一个修饰键（Ctrl/Alt/Shift/Win），否则会吞掉其他程序的按键。
          "hotkeys": {
            // 显示/隐藏对话框
            "toggleDialog": {{Scalar(config.Hotkeys.ToggleDialog)}},
            // 常驻 / 倒计时自动隐藏
            "toggleHold": {{Scalar(config.Hotkeys.ToggleHold)}}
          },

          // 声线包registry：运行时读这一段（改动无需重启，按 mtime 自动重载）。
          // kind: "lora-adapter" | "speaker-embedding" | "reference-audio"
          // path: 绝对路径，或相对 data 目录；character/avatar 供对话框显示说话人。
          "voicePacks": {{Packs(config)}}
        }
        """ + Environment.NewLine);
        File.Move(temp, target, overwrite: true);
    }

    /// <summary>One JSON string literal, quotes included, escaped by the serializer.</summary>
    private static string Scalar(string value) =>
        JsonSerializer.Serialize(value, WriteOptions);

    private static readonly JsonSerializerOptions WriteOptions = new()
    {
        WriteIndented = true,
        // The default encoder writes 汉字 and backslashes as \uXXXX into a file a human is
        // meant to read.
        Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
    };

    /// <summary>
    /// The packs section, re-indented to sit under its key. Written from the raw element so
    /// a field this class does not know about survives the round trip.
    /// </summary>
    private static string Packs(AppConfig config)
    {
        if (config.VoicePacks is not JsonElement packs) return "[]";
        return JsonSerializer.Serialize(packs, WriteOptions).Replace("\n", "\n  ");
    }

    /// <summary>
    /// Fold the old single-purpose files into this one. PURE: it reads and returns, and
    /// names the files it consumed so the caller can delete them AFTER the merged config is
    /// durable. Deleting them here - before the write that replaces them is proven - is how
    /// a failed save turns into a lost voice pack registry.
    /// </summary>
    private static (AppConfig Config, List<string> Consumed) Merge(string dataDir, AppConfig current)
    {
        var config = current;
        var moved = new List<string>();

        if (ReadJson(Path.Combine(dataDir, "dialog.json")) is JsonElement d)
        {
            config = config with
            {
                Dialog = config.Dialog with
                {
                    AnnotationAbove = d.TryGetProperty("annotationAbove", out var above) &&
                                      above.ValueKind == JsonValueKind.True,
                },
            };
            moved.Add("dialog.json");
        }

        if (ReadJson(Path.Combine(dataDir, "hotkeys.json")) is JsonElement h)
        {
            config = config with
            {
                Hotkeys = config.Hotkeys with
                {
                    ToggleDialog = Str(h, "toggleDialog") ?? config.Hotkeys.ToggleDialog,
                    ToggleHold = Str(h, "toggleHold") ?? config.Hotkeys.ToggleHold,
                },
            };
            moved.Add("hotkeys.json");
        }

        // Packs only move if this file does not already carry them: config.json is the
        // truth once written, and a stale voicepacks.json must never overwrite it.
        if (config.VoicePacks is null &&
            ReadJson(Path.Combine(dataDir, "voicepacks.json")) is JsonElement packs)
        {
            config = config with { VoicePacks = packs };
            moved.Add("voicepacks.json");
        }

        return (config, moved);
    }

    private static JsonElement? ReadJson(string path)
    {
        try
        {
            if (!File.Exists(path)) return null;
            using var document = JsonDocument.Parse(File.ReadAllText(path));
            return document.RootElement.Clone();
        }
        catch { return null; }
    }

    private static string? Str(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;
}
