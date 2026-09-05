using Microsoft.UI.Dispatching;
using System.Diagnostics;
using VoiceCoreTray.Services;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// The Hold lifecycle. Everything visible is delegated: <see cref="DialogWindow"/>
/// draws, <see cref="TextPresenter"/> paces, <see cref="AudioPresenter"/> plays. What
/// lives here is the state machine, the dwell policy and the backlog cursor, because
/// those are the only decisions that need to see all three.
///
/// The behaviour change from the old overlay: an utterance no longer plays and
/// vanishes. Text finishes, audio finishes, and then the dialog HOLDS with a
/// blinking indicator until the user (or an explicit dwell) dismisses it.
///
/// 历史对话 is a cursor over <see cref="DialogHistory"/>, not a window: wheel-up over
/// the box walks back through past utterances IN the box, wheel-down walks forward,
/// and passing the newest entry returns to the live line. A separate modal would take
/// focus, cover the thing it describes, and need its own copy of the layout.
/// </summary>
public sealed class DialogPresenter : IDialogPresenter, IDisposable
{
    private enum State
    {
        /// <summary>Nothing on screen.</summary>
        Hidden,
        /// <summary>Revealing text; audio may be playing under it.</summary>
        Typing,
        /// <summary>Text complete, audio still playing.</summary>
        Speaking,
        /// <summary>Done, staying put, indicator blinking. No timer runs.</summary>
        Holding,
        /// <summary>Done, auto-hide countdown running; hover freezes it.</summary>
        Counting,
        /// <summary>Fade-out in flight.</summary>
        Fading,
    }

    private readonly SubtitleOptions _options;
    private readonly DispatcherQueue _dispatcher;
    private readonly DialogWindow _window;
    private readonly TextPresenter _text;
    private readonly AudioPresenter _audio;
    private readonly DialogHistory _history = new();
    private readonly DialogMetrics _metrics;
    /// <summary>Hover polling while a countdown runs. The bar itself is a compositor
    /// animation; this only decides whether it is frozen.</summary>
    private readonly DispatcherQueueTimer _tick;
    /// <summary>One-shot: clears a transient band message back to the position readout.</summary>
    private readonly DispatcherQueueTimer _badgeClear;

    private State _state = State.Hidden;
    private bool _enabled = true;
    private bool _pinned;
    private bool _textDone;
    private bool _audioPlaying;
    private DialogUtterance? _current;
    /// <summary>Backlog cursor: 0 = the live line, n = n utterances back. Non-zero is
    /// the whole of "browsing 历史对话".</summary>
    private int _backtrack;
    /// <summary>Where the wheel has asked the cursor to go. Tracks <see cref="_backtrack"/>,
    /// but is advanced inside the hook callback (see <see cref="ScrollGesture"/>) so
    /// consecutive notches still accumulate while the steps themselves run later.</summary>
    private int _wheelTarget;
    /// <summary>Lines waiting for the current one to finish, with the timestamp they
    /// arrived at, so the wait can be reported.</summary>
    private readonly Queue<(DialogUtterance Utterance, long EnqueuedTicks)> _pending = new();

    public DialogPresenter(RuntimeClient runtime, SubtitleOptions options)
    {
        _options = options;
        _pinned = options.PinMode;
        _dispatcher = DispatcherQueue.GetForCurrentThread();

        _metrics = new DialogMetrics(runtime.LogDir);
        _window = new DialogWindow(runtime.DataDir, _metrics);
        _window.HistoryRequested += OpenHistory;
        _window.ReplayRequested += ReplayCurrentEntry;
        _window.CloseRequested += Hide;

        // The reveal sink is wrapped so the FIRST visible character can be timed: that,
        // against audioDoneMs, is what says whether text and voice are actually paired.
        _text = new TextPresenter(_dispatcher, (prefix, sourcePrefix) =>
        {
            if (_metrics.FirstRevealMs <= 0 && prefix.Length > 0)
                _metrics.FirstRevealMs = _metrics.UtteranceMs;
            _window.RevealText(prefix, sourcePrefix);
        });
        _audio = new AudioPresenter(_dispatcher, runtime, _metrics);

        _tick = _dispatcher.CreateTimer();
        _tick.Interval = TimeSpan.FromMilliseconds(100);
        _tick.Tick += (_, _) => OnTick();

        // Transient band messages ("nothing older", "audio released") clear themselves;
        // a status that stays is a status nobody reads.
        _badgeClear = _dispatcher.CreateTimer();
        _badgeClear.Interval = TimeSpan.FromMilliseconds(1600);
        _badgeClear.IsRepeating = false;
        _badgeClear.Tick += (_, _) => RestoreBadge();
    }

    public event EventHandler<DialogControlEventArgs>? ControlRequested;

    // -- IDialogPresenter ----------------------------------------------------

    /// <summary>
    /// Accept one utterance. A line that arrives while another is still speaking is
    /// QUEUED, not swapped in: its audio and its text belong together, and interrupting
    /// them mid-flight is what made a batch look like the text could not keep up with the
    /// voice. The queue is bounded and drops its OLDEST entry when full - a caption that
    /// is behind is worse than one that skipped a line.
    /// </summary>
    public void Show(DialogUtterance utterance)
    {
        var primary = utterance.DisplayText is { Length: > 0 } display ? display : utterance.Text;
        if (!_enabled || string.IsNullOrWhiteSpace(primary)) return;

        // The backlog records arrival order, not presentation order: the line was said.
        var source = utterance.Text is { Length: > 0 } text && text != primary ? text : null;
        if (utterance.AudioId.Length > 0)
            _history.Add(utterance.AudioId, primary, source, utterance.Character,
                utterance.AvatarPath, utterance.RubyPairs);

        if (_state is State.Typing or State.Speaking)
        {
            _pending.Enqueue((utterance, Stopwatch.GetTimestamp()));
            while (_pending.Count > DialogTheme.QueueCapacity)
            {
                _pending.Dequeue();
                _metrics.QueueDropped++;
            }
            return;
        }

        Present(utterance, queuedMs: 0);
    }

    /// <summary>Put a line up now: reset state, start its audio, start its reveal.</summary>
    private void Present(DialogUtterance utterance, double queuedMs)
    {
        var primary = utterance.DisplayText is { Length: > 0 } display ? display : utterance.Text;
        if (string.IsNullOrWhiteSpace(primary)) return;
        var source = utterance.Text is { Length: > 0 } text && text != primary ? text : null;

        _text.Cancel();
        _audio.Stop();
        _tick.Stop();

        // Keep the line, not its audio. `_current` outlives the utterance (Recall and
        // EffectiveDwell read it and nothing ever clears it), the bytes are handed to the
        // player from the local below and never read through here, so keeping them pins a
        // multi-megabyte clip in a tray that is meant to sit idle.
        _current = utterance with { Wav = null };
        _textDone = false;
        // A live line always wins over browsing: whoever is reading the backlog would
        // otherwise silently miss what was just said.
        _backtrack = _wheelTarget = 0;
        _metrics.MarkUtteranceStart(queuedMs, _pending.Count);

        _window.SetCharacter(utterance.Character ?? string.Empty, utterance.AvatarPath, null);
        _window.SetBrowsing(false);
        _window.BeginUtterance(primary, source, utterance.RubyPairs);
        _window.StopCountdown();
        _window.SetWaiting(false);

        // Audio first: it is the clock the text is paced against, and every millisecond
        // of visual setup done before it starts is a millisecond of drift.
        double? audioSeconds = null;
        if (utterance.Autoplay && utterance.Wav is { Length: > 0 } wav)
        {
            audioSeconds = _audio.Play(wav, utterance.AudioId, OnAudioFinished);
            _audioPlaying = true;
        }
        else
        {
            _audioPlaying = false;
        }

        _state = State.Typing;

        // The typewriter is paced against the audio; the other presets are fixed-length
        // animations over an already-placed layout, so they only need to say when they are
        // done.
        if (_window.Reveal == RevealStyle.Typewriter)
        {
            double revealSeconds = RevealSeconds(primary.Length, audioSeconds);
            _metrics.AudioMs = (audioSeconds ?? 0) * 1000;
            _metrics.RevealBudgetMs = revealSeconds * 1000;
            _text.Play(primary, source, revealSeconds, OnTextFinished);
        }
        else
        {
            _metrics.AudioMs = (audioSeconds ?? 0) * 1000;
            _window.RevealComplete(OnTextFinished);
        }
    }

    public void Hide()
    {
        if (_state is State.Hidden or State.Fading) return;
        BeginFade();
    }

    public void SetEnabled(bool enabled)
    {
        _enabled = enabled;
        if (enabled) return;

        _pending.Clear();
        _text.Cancel();
        _audio.Stop();
        _tick.Stop();
        _state = State.Hidden;
        _window.HideNow();
    }

    /// <summary>Raised when the mode changed from inside the dialog, so a menu that
    /// mirrors it does not drift out of sync.</summary>
    public event Action<bool>? PinnedChanged;

    /// <summary>
    /// 常驻 (true) or 倒计时 (false). One state, two entry points: the tray menu and
    /// <see cref="ToggleHold"/>'s hotkey. The old second flag (a per-utterance hold
    /// override) is gone - two overlapping ways to say "do not auto-hide" meant the
    /// dialog could be in a mode the menu did not show.
    /// </summary>
    public bool Pinned
    {
        get => _pinned;
        set
        {
            if (_pinned == value) return;
            _pinned = value;

            if (value && _state == State.Counting) EnterHold();
            else if (!value && _state == State.Holding && _backtrack == 0)
                StartCountdown(EffectiveDwell() ?? DialogTheme.DefaultDwellSeconds);

            PinnedChanged?.Invoke(value);
        }
    }

    public void ResetPosition() => _window.ResetPosition();

    public void ToggleVisibility()
    {
        if (_state is State.Hidden or State.Fading) Recall();
        else Hide();
    }

    /// <summary>The one mode hotkey (Ctrl+Alt+H). It reports where it landed in the
    /// band, because a mode toggle with no readout is a coin flip.</summary>
    public void ToggleHold()
    {
        Pinned = !Pinned;
        FlashBadge(_pinned ? "常驻" : "倒计时");
    }

    /// <summary>Menu, hotkey and the band's 历史 control: step one utterance back, or
    /// return to the live line if already browsing. A control that reports nothing when
    /// it cannot act is a control that looks broken, so a dead end says so in the
    /// band.</summary>
    public void OpenHistory()
    {
        if (_backtrack > 0)
        {
            Backtrack(0);
            return;
        }
        if (!Backtrack(1)) FlashBadge("没有更早的对话");
    }

    /// <summary>Wheel over the box: up walks back through the backlog, down walks
    /// forward. Consumed only when it actually moved the cursor, so a wheel over the box
    /// with nothing behind it still scrolls whatever is underneath.
    ///
    /// This runs INSIDE the <c>WH_MOUSE_LL</c> callback (<see cref="WheelGestureWatcher"/>)
    /// with the raw input thread blocked until it returns, so it decides consume or
    /// pass-through and NOTHING else. The step itself - portrait decode, BuildLines,
    /// UpdateLayout, MoveAndResize (4-8 ms on its own), two storyboards - is dispatched:
    /// done here it stalled mouse input for the whole desktop once per notch, and a
    /// callback over LowLevelHooksTimeout (300 ms; a first browse pays JIT for the layout
    /// plus a cold-disk avatar decode) is silently unhooked by Windows.
    /// </summary>
    public bool ScrollGesture(int screenX, int screenY, int wheelDelta)
    {
        // The only work kept inline: HitTestScreen early-returns while the dialog is not
        // shown, so every wheel event elsewhere on the desktop stays a pass-through.
        if (wheelDelta == 0 || !_window.HitTestScreen(screenX, screenY)) return false;

        // Stepped from the INTENT cursor, not from _backtrack: _backtrack only catches up
        // when the dispatched step runs, so notches spun back to back must accumulate here.
        int target = _wheelTarget + (wheelDelta > 0 ? 1 : -1);
        if (target >= 0 && target < _history.Entries.Count)
        {
            _wheelTarget = target;
            // Only the step that is still current runs: a fast spin queues one item per
            // notch, and replaying every intermediate entry would be N full relayouts to
            // land where the last notch already points. A stale item is also how a live
            // utterance arriving mid-spin used to get overwritten - Present resets the
            // cursor, so its step is skipped instead of re-entering the backlog.
            _dispatcher.TryEnqueue(() => { if (_wheelTarget == target) Backtrack(target); });
            return true;
        }

        // Wheel-up at the oldest entry: say so rather than silently doing nothing, and
        // still consume it - scrolling the window underneath would be a surprise.
        if (wheelDelta > 0)
        {
            _dispatcher.TryEnqueue(() =>
                FlashBadge(_history.Entries.Count > 1 ? "已到最早的对话" : "没有更早的对话"));
            return true;
        }
        return false;
    }

    /// <summary>
    /// Move the backlog cursor. 0 is the live line; n is n utterances back. Returns
    /// whether anything moved, which is what decides if the gesture was consumed.
    ///
    /// Browsing implies Hold: an auto-hide countdown running under someone reading the
    /// backlog would take the dialog away mid-sentence.
    /// </summary>
    private bool Backtrack(int index)
    {
        var entries = _history.Entries;
        if (index < 0) index = 0;
        if (index >= entries.Count) return false;
        if (index == _backtrack) return false;

        _backtrack = _wheelTarget = index;
        var entry = entries[index];
        // The portrait belongs to the line, not to the dialog: whoever said THIS is who
        // the box shows while it is on screen.
        _window.SetCharacter(entry.Character ?? string.Empty, entry.AvatarPath, null);
        _window.ShowLine(entry.DisplayText, entry.SourceText, entry.RubyPairs);
        _window.SetHistoryBadge(index == 0 ? null : PositionText(index));
        _window.SetBrowsing(index > 0);

        _text.Cancel();
        _textDone = true;

        // Browsing abandons the line that was speaking, and its batch goes with it: left
        // in place nothing ever drains _pending, because EnterHold below makes the state
        // Holding and OnAudioFinished only settles from Typing/Speaking. Those lines would
        // resurface after the NEXT utterance as the newest thing said, each one pinning its
        // WAV bytes in an idle tray until then.
        if (_state is State.Typing or State.Speaking)
        {
            _metrics.QueueDropped += _pending.Count;
            _pending.Clear();
            // The abandoned voice must not keep talking under the browsed line. Stop bumps
            // AudioPresenter's generation, which suppresses that clip's completion
            // callback, so OnAudioFinished will never clear this flag for us.
            _audio.Stop();
            _audioPlaying = false;
        }

        EnterHold();
        return true;
    }

    /// <summary>Click while browsing: replay that utterance's audio if the runtime's
    /// spool still has it. The spool expires by age and size and is wiped on restart, so
    /// text routinely outlives audio; a failed replay says so in the band instead of doing
    /// nothing. Rapid repeat clicks are dropped by the single-flight in
    /// <see cref="AudioPresenter"/> and reported, rather than each starting its own
    /// download and player.</summary>
    private async void ReplayCurrentEntry()
    {
        if (_backtrack <= 0 || _backtrack >= _history.Entries.Count) return;

        var entry = _history.Entries[_backtrack];
        FlashBadge(await _audio.ReplayAsync(entry.AudioId) switch
        {
            AudioPresenter.Replay.Started => "重播",
            AudioPresenter.Replay.Busy => "重播中",
            _ => "语音已释放",
        });
    }

    /// <summary>
    /// Oldest is 1, newest is the total: the backlog reads like a transcript, not like a
    /// distance from now. The cursor counts the other way (0 = live), so this inverts it.
    /// </summary>
    private string PositionText(int index)
    {
        int total = _history.Entries.Count - 1;
        return $"{total - index + 1} / {total} · {_history.Entries[index].Stamp}";
    }

    /// <summary>Transient band message that falls back to the position readout.</summary>
    private void FlashBadge(string message)
    {
        _badgeClear.Stop();
        _window.SetHistoryBadge(_backtrack > 0 ? $"{PositionText(_backtrack)} · {message}" : message);
        _badgeClear.Start();
    }

    private void RestoreBadge() =>
        _window.SetHistoryBadge(_backtrack > 0 ? PositionText(_backtrack) : null);

    public void RequestControl(DialogControl control, double value, string? detail = null) =>
        ControlRequested?.Invoke(this, new DialogControlEventArgs(control, value, detail));

    // -- lifecycle -----------------------------------------------------------

    private void OnTextFinished()
    {
        _textDone = true;
        _metrics.RevealDoneMs = _metrics.UtteranceMs;
        if (_state is not (State.Typing or State.Speaking)) return;
        if (_audioPlaying) _state = State.Speaking;
        else Settle();
    }

    private void OnAudioFinished()
    {
        _audioPlaying = false;
        _metrics.AudioDoneMs = _metrics.UtteranceMs;
        if (_state is State.Typing or State.Speaking && _textDone) Settle();
    }

    /// <summary>
    /// Text and audio are done. If lines are waiting, the next one goes up immediately -
    /// that is what keeps a batch in order and in sync instead of each line cutting the
    /// previous one short. Otherwise: hold, or start the auto-hide countdown. Either way
    /// this utterance's cost and its audio/text timings are written out (see
    /// <see cref="DialogMetrics"/>): the reveal is over, so the numbers describe a
    /// complete line.
    /// </summary>
    private void Settle()
    {
        int chars = (_current?.DisplayText ?? _current?.Text)?.Length ?? 0;

        if (_pending.Count > 0)
        {
            var (next, enqueuedTicks) = _pending.Dequeue();
            double queuedMs = (Stopwatch.GetTimestamp() - enqueuedTicks) * 1000.0 / Stopwatch.Frequency;
            _metrics.Flush("queued-next", chars);
            Present(next, queuedMs);
            return;
        }

        if (EffectiveDwell() is double dwell) StartCountdown(dwell);
        else EnterHold();

        _metrics.Flush("settled", chars);
    }

    /// <summary>
    /// How long to stay after the line lands. Null means "never auto-hide", which now
    /// happens only in 常驻 (<see cref="Pinned"/>): the DEFAULT is a countdown, so a
    /// caption the user never touches clears itself off the desktop. Order: 常驻 wins,
    /// then a forced CLI dwell, then the runtime's per-utterance hint, then the default.
    /// </summary>
    private double? EffectiveDwell()
    {
        if (_pinned) return null;
        if (_options.ForcedDwell is double forced) return forced;
        if (_current?.DisplaySeconds is double pushed && pushed > 0) return pushed;
        return DialogTheme.DefaultDwellSeconds;
    }

    private void EnterHold()
    {
        _tick.Stop();
        _state = State.Holding;
        _window.StopCountdown();
        _window.SetWaiting(true);
    }

    /// <summary>
    /// Hand the whole dwell to one compositor animation and keep only the hover poll:
    /// the bar's smoothness is then the compositor's problem, not a timer's, and the
    /// remaining time lives in the paused animation instead of a deadline this class
    /// would have to keep patching.
    /// </summary>
    private void StartCountdown(double seconds)
    {
        _state = State.Counting;
        _window.SetWaiting(true);
        _window.StartCountdown(seconds, OnCountdownElapsed);
        _tick.Start();
    }

    private void OnCountdownElapsed()
    {
        if (_state == State.Counting) BeginFade();
    }

    /// <summary>Hover freeze only. Everything else about the countdown is animated.</summary>
    private void OnTick()
    {
        if (_state != State.Counting)
        {
            _tick.Stop();
            return;
        }
        _window.SetCountdownFrozen(_window.IsCursorOverBox);
    }

    /// <summary>Dismissal, from a click, the 关闭 control, a hotkey or an elapsed
    /// countdown. It drops whatever was queued: "go away" means the batch too, otherwise
    /// the next line would pop back up a moment after the user closed the dialog.</summary>
    private void BeginFade()
    {
        if (_state is State.Typing or State.Speaking)
        {
            _metrics.Truncated = true;
            _metrics.Flush("dismissed", (_current?.DisplayText ?? _current?.Text)?.Length ?? 0);
        }

        _pending.Clear();
        _tick.Stop();
        _text.Cancel();
        _audio.Stop();
        _audioPlaying = false;
        _state = State.Fading;
        _window.FadeOut(() =>
        {
            if (_state == State.Fading) _state = State.Hidden;
        });
    }

    /// <summary>Hotkey recall: put the last utterance back without replaying it. An
    /// explicit recall means "I want to read this", so it holds even when a dwell is
    /// configured.</summary>
    private void Recall()
    {
        if (!_enabled || _current is null) return;

        var primary = _current.DisplayText is { Length: > 0 } display ? display : _current.Text;
        if (string.IsNullOrWhiteSpace(primary)) return;
        var source = _current.Text is { Length: > 0 } text && text != primary ? text : null;

        _text.Cancel();
        _textDone = true;
        _audioPlaying = false;
        _backtrack = _wheelTarget = 0;
        // Recall is reading, not speaking: the full line at once, no growth motion.
        _window.ShowLine(primary, source, _current?.RubyPairs);
        _window.SetHistoryBadge(null);
        EnterHold();
    }

    /// <summary>
    /// How long the reveal should take. With audio, the line lands just before the
    /// voice stops; without it, per-character pacing clamped at both ends so a short
    /// line is not sluggish and a 400-character line is not interminable.
    /// </summary>
    private static double RevealSeconds(int chars, double? audioSeconds)
    {
        double budget = audioSeconds is double seconds && seconds > 0.1
            ? seconds * DialogTheme.TypeAudioRatio
            : chars * DialogTheme.TypeSecondsPerChar;
        return Math.Clamp(budget, DialogTheme.TypeMinSeconds, DialogTheme.TypeMaxSeconds);
    }

    // -- probe modes ---------------------------------------------------------

    /// <summary>Headless layout matrix (--subtitle-selftest).</summary>
    public Task RunSelfTestAsync(string reportPath) => _window.RunSelfTestAsync(reportPath);

    public void Dispose()
    {
        _tick.Stop();
        _text.Cancel();
        _audio.Stop();
    }
}
