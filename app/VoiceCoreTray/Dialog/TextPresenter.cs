using Microsoft.UI.Dispatching;
using System.Text;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// Typewriter pacing over an append-only buffer, revealing the utterance AND its source
/// line together.
///
/// The runtime emits one <c>speech</c> event with the complete strings, so this is
/// client-side pacing and not a stream decoder. It is still written as a sink
/// (<see cref="Begin"/> / <see cref="Push"/> / <see cref="Seal"/>) because pacing by a
/// *rate* rather than a precomputed per-character schedule is what lets a future chunked
/// source extend the buffer mid-flight without recomputing anything. <see cref="Play"/>
/// is the whole-string convenience over that seam.
///
/// The two lines are mapped by FRACTION, not by character: they are different languages,
/// and nothing in the pipeline provides a token alignment, so the honest structural
/// mapping is "both lines are the same fraction revealed". One clock drives both, they
/// finish together, and the pair reads as one utterance instead of a translation stapled
/// under a caption.
///
/// The reveal is computed from elapsed time, not accumulated per tick, so a dropped frame
/// or a busy UI thread cannot make the line drift behind the audio.
/// </summary>
internal sealed class TextPresenter
{
    private readonly DispatcherQueueTimer _timer;
    private readonly Action<string, string> _reveal;
    private readonly StringBuilder _buffer = new();

    private string _source = string.Empty;
    private double _charsPerSecond;
    private DateTimeOffset _started;
    private int _revealed;
    private bool _sealed;
    private Action? _finished;

    public TextPresenter(DispatcherQueue dispatcher, Action<string, string> reveal)
    {
        _reveal = reveal;
        _timer = dispatcher.CreateTimer();
        _timer.Interval = DialogTheme.TypeTick;
        _timer.IsRepeating = true;
        _timer.Tick += (_, _) => Pump();
    }

    public bool IsRunning { get; private set; }

    /// <summary>
    /// Reveal <paramref name="text"/> over roughly <paramref name="seconds"/>, with
    /// <paramref name="source"/> kept at the same fraction underneath it.
    /// </summary>
    public void Play(string text, string? source, double seconds, Action onFinished)
    {
        Begin(text.Length / Math.Max(seconds, 0.05), onFinished);
        _source = source ?? string.Empty;
        Push(text);
        Seal();
    }

    /// <summary>Start an empty reveal at a fixed rate. Stream seam.</summary>
    public void Begin(double charsPerSecond, Action onFinished)
    {
        Cancel();
        _buffer.Clear();
        _charsPerSecond = Math.Max(charsPerSecond, 1.0);
        _revealed = 0;
        _sealed = false;
        _finished = onFinished;
        _started = DateTimeOffset.UtcNow;
        IsRunning = true;
        _reveal(string.Empty, string.Empty);
        _timer.Start();
    }

    /// <summary>Append text to reveal. Stream seam.</summary>
    public void Push(string chunk)
    {
        if (chunk.Length == 0) return;
        _buffer.Append(chunk);
    }

    /// <summary>No more text is coming; the reveal may finish. Stream seam.</summary>
    public void Seal()
    {
        _sealed = true;
        if (_revealed >= _buffer.Length) Drain();
    }

    /// <summary>Abandon the reveal without reporting completion.</summary>
    public void Cancel()
    {
        _timer.Stop();
        IsRunning = false;
        _finished = null;
    }

    private void Pump()
    {
        if (!IsRunning) { _timer.Stop(); return; }

        double elapsed = (DateTimeOffset.UtcNow - _started).TotalSeconds;
        int want = Math.Min((int)(elapsed * _charsPerSecond), _buffer.Length);
        if (want > _revealed)
        {
            _revealed = AlignForward(want);
            Emit();
        }
        if (_sealed && _revealed >= _buffer.Length) Drain();
    }

    /// <summary>Reveal everything buffered and, once sealed, report completion.</summary>
    private void Drain()
    {
        if (!IsRunning) return;
        if (_revealed < _buffer.Length)
        {
            _revealed = _buffer.Length;
            Emit();
        }
        if (!_sealed) return;

        _timer.Stop();
        IsRunning = false;
        var finished = _finished;
        _finished = null;
        finished?.Invoke();
    }

    /// <summary>
    /// Push both lines at the same fraction. The source line is cut at the same relative
    /// position as the revealed one and rounded UP, so it is never a character behind:
    /// the pair should look like one thing arriving, and a lagging second line is exactly
    /// what made it look like two.
    /// </summary>
    private void Emit()
    {
        string primary = _buffer.ToString(0, _revealed);
        if (_source.Length == 0)
        {
            _reveal(primary, string.Empty);
            return;
        }

        double fraction = _buffer.Length == 0 ? 1 : (double)_revealed / _buffer.Length;
        int take = Math.Min(_source.Length, (int)Math.Ceiling(fraction * _source.Length));
        if (take > 0 && take < _source.Length && char.IsHighSurrogate(_source[take - 1])) take++;
        _reveal(primary, _source[..take]);
    }

    /// <summary>Never cut a surrogate pair in half: an emoji would render as a box
    /// for one frame.</summary>
    private int AlignForward(int count) =>
        count > 0 && count < _buffer.Length && char.IsHighSurrogate(_buffer[count - 1])
            ? count + 1
            : count;
}
