namespace VoiceCoreTray.Dialog;

/// <summary>
/// One utterance to present. This mirrors what the runtime's <c>speech</c> event
/// plus its audio endpoint already produced (see docs/api.md); nothing here is
/// synthesized by the UI, and <c>Wav</c> is the bytes the tray already fetched.
/// </summary>
/// <param name="AudioId">Spool id, used for replay. Empty for local previews.</param>
/// <param name="DisplayText">What a human reads. Falls back to <paramref name="Text"/>.</param>
/// <param name="Text">What was spoken (source language); shown as the dim second line.</param>
/// <param name="Wav">Already-fetched WAV bytes, or null.</param>
/// <param name="DisplaySeconds">Runtime-supplied auto-hide hint; null = the default dwell.</param>
/// <param name="Autoplay">Whether the tray's autoplay toggle is on.</param>
/// <param name="Character">Speaker's display name, resolved from the utterance's voice
/// pack. Carried per utterance, not set globally: the portrait has to follow the line
/// it belongs to, including when the backlog is walked back through.</param>
/// <param name="AvatarPath">Absolute path to the speaker's portrait, or null.</param>
/// <param name="RubyPairs">Segment-by-segment alignment between <paramref name="DisplayText"/>
/// and <paramref name="Text"/>, as the caller supplied it. The dialog renders these directly:
/// only the producer knows which fragment means which, and a client that guesses a mapping
/// between two languages that do not line up positionally renders a correspondence that is
/// not there.</param>
public sealed record DialogUtterance(
    string AudioId,
    string? DisplayText,
    string? Text,
    byte[]? Wav,
    double? DisplaySeconds,
    bool Autoplay,
    string? Character = null,
    string? AvatarPath = null,
    IReadOnlyList<(string Base, string Ruby)>? RubyPairs = null);

/// <summary>Reserved control intents. No runtime route exists for any of them.</summary>
public enum DialogControl
{
    /// <summary>Playback / typing speed multiplier.</summary>
    Speed,
    /// <summary>Playback volume, 0..1.</summary>
    Volume,
    /// <summary>A branch option was chosen; the index is the value.</summary>
    BranchChoice,
}

/// <summary>Payload of <see cref="IDialogPresenter.ControlRequested"/>.</summary>
public sealed class DialogControlEventArgs(DialogControl control, double value, string? detail)
    : EventArgs
{
    public DialogControl Control { get; } = control;
    public double Value { get; } = value;
    public string? Detail { get; } = detail;
}

/// <summary>
/// The dialog surface the tray talks to. The tray owns the runtime connection and
/// global input; the presenter owns everything on screen. Nothing in here exposes
/// a window, a XAML type or a Win32 handle, so a different look (or a different UI
/// framework) is a second implementation rather than a rewrite of the tray.
/// </summary>
public interface IDialogPresenter
{
    /// <summary>Present one utterance: reveal the text, play the audio, then hold.</summary>
    void Show(DialogUtterance utterance);

    /// <summary>Fade out now. Idempotent.</summary>
    void Hide();

    /// <summary>Master switch (tray menu). False hides and drops incoming utterances.</summary>
    void SetEnabled(bool enabled);

    /// <summary>常驻: never auto-hide. The tray menu mirrors it and
    /// <see cref="ToggleHold"/> is the hotkey onto the same state.</summary>
    bool Pinned { get; set; }

    /// <summary>Forget the dragged anchor and snap back to bottom-center.</summary>
    void ResetPosition();

    /// <summary>Hotkey: hide if visible, otherwise recall the last utterance.</summary>
    void ToggleVisibility();

    /// <summary>Hotkey (Ctrl+Alt+H): 常驻 &lt;-&gt; 倒计时. The dialog reports which mode it
    /// landed in, so one key is enough for both directions.</summary>
    void ToggleHold();

    /// <summary>Step into the backlog, or back out of it if already browsing (the
    /// dialog's 历史 control and the tray menu).</summary>
    void OpenHistory();

    /// <summary>A global wheel event over the box: up walks back through the backlog,
    /// down walks forward, like a Galgame log. Screen coordinates, raw wheel delta. True
    /// means the gesture was consumed and must not also scroll what is underneath.</summary>
    bool ScrollGesture(int screenX, int screenY, int wheelDelta);

    /// <summary>Control seam. Raised by <see cref="RequestControl"/>; there is no
    /// runtime route for speed, volume or branch choice, so nothing forwards it.</summary>
    event EventHandler<DialogControlEventArgs>? ControlRequested;

    /// <summary>Entry point for a future control surface: raises
    /// <see cref="ControlRequested"/> and does nothing else.</summary>
    void RequestControl(DialogControl control, double value, string? detail = null);
}
