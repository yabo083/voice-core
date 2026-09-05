using Microsoft.UI.Dispatching;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// Re-reads <c>config.json</c> when it changes on disk and hands the result to whoever owns
/// those sections. It is what makes both settings surfaces honest: a colour, a reveal mode,
/// a dwell or a hotkey takes effect on the next line instead of at the next restart, and
/// editing the file in notepad works exactly as well as the panel does.
///
/// An mtime poll, not a <c>FileSystemWatcher</c>. Three reasons, in order:
///   * the runtime already answers the same question about the same file the same way
///     (<c>Registry::reload_if_changed</c> in src/packs.rs), and two different mechanisms
///     watching one file is one too many to reason about when they disagree;
///   * a save is not one event. The panel splices bytes into the file, the tray renames a
///     sibling over it and notepad truncates and rewrites, so change notifications arrive
///     one, two or three times per save and sometimes before all the bytes are there. Every
///     FileSystemWatcher user ends up writing this debounce anyway, and then owns both;
///   * it costs one <c>GetLastWriteTimeUtc</c> per second - a single stat on a file the OS
///     has cached - on a thread that is otherwise idle between utterances.
///
/// The tick runs on the UI thread deliberately: applying a change re-themes XAML, and
/// <c>RegisterHotKey</c> must be called from the thread that owns the window.
/// </summary>
internal sealed class ConfigWatcher : IDisposable
{
    /// <summary>Poll interval: fast enough that a settings page feels immediate, slow enough
    /// to be free.</summary>
    private static readonly TimeSpan Interval = TimeSpan.FromSeconds(1);

    private readonly string _dataDir;
    private readonly string _file;
    private readonly Action<AppConfig> _apply;
    private readonly DispatcherQueueTimer _timer;
    private DateTime _stamp;

    public ConfigWatcher(string dataDir, DispatcherQueue dispatcher, Action<AppConfig> apply)
    {
        _dataDir = dataDir;
        _file = Path.Combine(dataDir, AppConfig.FileName);
        _apply = apply;
        // The caller has already applied what the file says right now, so only later changes
        // are ours to report.
        _stamp = Stamp();

        _timer = dispatcher.CreateTimer();
        _timer.Interval = Interval;
        _timer.IsRepeating = true;
        _timer.Tick += (_, _) => Poll();
        _timer.Start();
    }

    /// <summary>The file's mtime, or <c>default</c> when it is not there (or not readable
    /// this instant). An absent file is a state to notice a change OUT of, not to throw on.</summary>
    private DateTime Stamp()
    {
        try { return File.GetLastWriteTimeUtc(_file); }
        catch { return default; }
    }

    private void Poll()
    {
        var stamp = Stamp();
        if (stamp == _stamp) return;

        // Sampled BEFORE the read and accepted only after it parsed. Both halves matter: a
        // write caught half-finished does not parse, and re-reading on the next tick is the
        // whole recovery - while accepting the mtime of a failed read would pin the old
        // settings against a file that has visibly changed.
        if (AppConfig.Read(_dataDir) is not AppConfig config) return;
        _stamp = stamp;
        _apply(config);
    }

    public void Dispose() => _timer.Stop();
}
