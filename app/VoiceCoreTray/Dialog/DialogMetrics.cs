using System.Diagnostics;
using System.Globalization;
using System.Text;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// What the dialog actually costs, per utterance, written where it can be read after
/// the fact.
///
/// This exists because a UI performance problem here is invisible from the outside: the
/// dialog's low-level mouse hook lives on the tray's UI thread, so ANY stall on that
/// thread delays mouse input for the whole desktop. The symptom is "the cursor stutters",
/// which says nothing about the cause. So the two things that can stall it are counted
/// and timed — the per-frame measure/resize loop that follows the text, and audio replay —
/// plus <see cref="MaxTickGapMs"/>, which is the gap between when a 16 ms timer was due
/// and when it actually ran. That number IS the UI-thread stall, measured.
///
/// One JSON line per utterance into <c>logs/dialog.jsonl</c>, matching the runtime's
/// <c>metrics.jsonl</c> convention (see AGENTS.md observability rule): no dependency, no
/// server, greppable.
/// </summary>
internal sealed class DialogMetrics(string logDir)
{
    private readonly Stopwatch _clock = Stopwatch.StartNew();
    /// <summary>Restarted when an utterance is presented, so every timing below is an
    /// offset from "the line went up", which is the only reference point that makes
    /// audio-versus-text sync a number instead of an impression.</summary>
    private readonly Stopwatch _utterance = new();

    public long Reveals;
    public long Measures;
    public long Resizes;
    public long RegionUpdates;
    public long GrowFrames;
    public long ReplayFetches;
    public long ReplayCacheHits;
    public long ReplaysSkipped;
    public double MeasureMs;
    public double ResizeMs;
    /// <summary>Worst overrun of a growth frame timer, in ms. The UI-thread stall.</summary>
    public double MaxTickGapMs;

    // -- audio/text sync -----------------------------------------------------

    /// <summary>Clip length from the WAV header, ms. 0 when the line had no audio.</summary>
    public double AudioMs;
    /// <summary>Which reveal preset ran. A voice pack decides this per line, so a log line
    /// that does not name it cannot explain what was seen on screen.</summary>
    public RevealStyle RevealMode;
    /// <summary>Reveal budget handed to the typewriter, ms.</summary>
    public double RevealBudgetMs;
    /// <summary>First revealed character, ms after the line went up.</summary>
    public double FirstRevealMs;
    /// <summary>Text complete, ms after the line went up.</summary>
    public double RevealDoneMs;
    /// <summary>Audio finished, ms after the line went up.</summary>
    public double AudioDoneMs;
    /// <summary>How long this line waited behind others before being presented, ms.</summary>
    public double QueuedMs;
    /// <summary>Lines still waiting when this one started.</summary>
    public long QueueDepth;
    /// <summary>Whether a newer line replaced this one before it finished.</summary>
    public bool Truncated;
    /// <summary>Waiting lines discarded because the queue was full.</summary>
    public long QueueDropped;

    public void MarkUtteranceStart(double queuedMs, int queueDepth)
    {
        _utterance.Restart();
        QueuedMs = queuedMs;
        QueueDepth = queueDepth;
    }

    public double UtteranceMs => _utterance.Elapsed.TotalMilliseconds;

    /// <summary>Scoped stopwatch for one measured operation.</summary>
    public readonly struct Span(DialogMetrics owner, long startTicks, bool resize) : IDisposable
    {
        public void Dispose()
        {
            double ms = (Stopwatch.GetTimestamp() - startTicks) * 1000.0 / Stopwatch.Frequency;
            if (resize)
            {
                owner.Resizes++;
                owner.ResizeMs += ms;
            }
            else
            {
                owner.Measures++;
                owner.MeasureMs += ms;
            }
        }
    }

    public Span TimeMeasure() => new(this, Stopwatch.GetTimestamp(), resize: false);
    public Span TimeResize() => new(this, Stopwatch.GetTimestamp(), resize: true);

    /// <summary>Record how late a frame timer was. <paramref name="dueMs"/> is its
    /// interval; anything much larger means the UI thread was busy elsewhere.</summary>
    public void RecordTick(double elapsedMs, double dueMs)
    {
        double gap = elapsedMs - dueMs;
        if (gap > MaxTickGapMs) MaxTickGapMs = gap;
    }

    /// <summary>
    /// Append one line and reset the counters. Best effort on purpose: losing a metrics
    /// line must never take the dialog with it.
    /// </summary>
    public void Flush(string reason, int chars)
    {
        try
        {
            var line = new StringBuilder(256);
            line.Append(CultureInfo.InvariantCulture, $"{{\"ts\":\"{DateTime.Now:O}\"");
            line.Append(CultureInfo.InvariantCulture, $",\"reason\":\"{reason}\"");
            line.Append(CultureInfo.InvariantCulture, $",\"chars\":{chars}");
            line.Append(CultureInfo.InvariantCulture, $",\"reveals\":{Reveals}");
            line.Append(CultureInfo.InvariantCulture, $",\"growFrames\":{GrowFrames}");
            line.Append(CultureInfo.InvariantCulture, $",\"measures\":{Measures}");
            line.Append(CultureInfo.InvariantCulture, $",\"measureMs\":{MeasureMs:0.##}");
            line.Append(CultureInfo.InvariantCulture, $",\"resizes\":{Resizes}");
            line.Append(CultureInfo.InvariantCulture, $",\"resizeMs\":{ResizeMs:0.##}");
            line.Append(CultureInfo.InvariantCulture, $",\"regionUpdates\":{RegionUpdates}");
            line.Append(CultureInfo.InvariantCulture, $",\"replayFetches\":{ReplayFetches}");
            line.Append(CultureInfo.InvariantCulture, $",\"replayCacheHits\":{ReplayCacheHits}");
            line.Append(CultureInfo.InvariantCulture, $",\"replaysSkipped\":{ReplaysSkipped}");
            line.Append(CultureInfo.InvariantCulture, $",\"maxTickGapMs\":{MaxTickGapMs:0.##}");
            line.Append(CultureInfo.InvariantCulture, $",\"audioMs\":{AudioMs:0}");
            line.Append(CultureInfo.InvariantCulture,
                $",\"reveal\":\"{RevealMode.ToString().ToLowerInvariant()}\"");
            line.Append(CultureInfo.InvariantCulture, $",\"revealBudgetMs\":{RevealBudgetMs:0}");
            line.Append(CultureInfo.InvariantCulture, $",\"firstRevealMs\":{FirstRevealMs:0}");
            line.Append(CultureInfo.InvariantCulture, $",\"revealDoneMs\":{RevealDoneMs:0}");
            line.Append(CultureInfo.InvariantCulture, $",\"audioDoneMs\":{AudioDoneMs:0}");
            line.Append(CultureInfo.InvariantCulture, $",\"queuedMs\":{QueuedMs:0}");
            line.Append(CultureInfo.InvariantCulture, $",\"queueDepth\":{QueueDepth}");
            line.Append(CultureInfo.InvariantCulture, $",\"queueDropped\":{QueueDropped}");
            line.Append(CultureInfo.InvariantCulture, $",\"windowMs\":{_clock.Elapsed.TotalMilliseconds:0}}}");

            Directory.CreateDirectory(logDir);
            File.AppendAllText(Path.Combine(logDir, "dialog.jsonl"), line.AppendLine().ToString());
        }
        catch { /* metrics are not worth an exception on the UI thread */ }

        Reveals = Measures = Resizes = RegionUpdates = GrowFrames = 0;
        ReplayFetches = ReplayCacheHits = ReplaysSkipped = 0;
        MeasureMs = ResizeMs = MaxTickGapMs = 0;
        AudioMs = RevealBudgetMs = FirstRevealMs = RevealDoneMs = AudioDoneMs = QueuedMs = 0;
        RevealMode = RevealStyle.Typewriter;
        QueueDepth = QueueDropped = 0;
        Truncated = false;
        _clock.Restart();
    }

    /// <summary>Single-line summary for the self-test report.</summary>
    public string Summary() => string.Create(CultureInfo.InvariantCulture,
        $"reveals\t{Reveals}\tgrow_frames\t{GrowFrames}\tmeasures\t{Measures}\tmeasure_ms\t{MeasureMs:0.##}\t" +
        $"resizes\t{Resizes}\tresize_ms\t{ResizeMs:0.##}\tregion_updates\t{RegionUpdates}\t" +
        $"max_tick_gap_ms\t{MaxTickGapMs:0.##}");
}
