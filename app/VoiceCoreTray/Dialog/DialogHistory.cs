namespace VoiceCoreTray.Dialog;

/// <summary>
/// One past utterance, kept so the dialog can walk back through what was said. Text
/// and speaker only: the runtime's spool expires audio by age and by total size and
/// wipes itself when the runtime restarts (src/spool.rs), so text routinely outlives
/// its audio and a replay attempt is the only honest availability check.
///
/// The speaker rides along with the line. A single "current character" on the dialog
/// would show whoever spoke last while the box displays a line from someone else.
/// </summary>
public sealed class HistoryEntry
{
    public HistoryEntry(string audioId, string displayText, string? sourceText,
        string? character, string? avatarPath, IReadOnlyList<(string Base, string Ruby)>? rubyPairs)
    {
        AudioId = audioId;
        DisplayText = displayText;
        SourceText = sourceText is null || sourceText == displayText ? string.Empty : sourceText;
        Character = character;
        AvatarPath = avatarPath;
        RubyPairs = rubyPairs;
        Stamp = DateTime.Now.ToString("HH:mm:ss");
    }

    public string AudioId { get; }
    public string Stamp { get; }
    public string DisplayText { get; }
    public string SourceText { get; }
    public string? Character { get; }
    public string? AvatarPath { get; }
    /// <summary>The caller's alignment, kept so a backtracked line is laid out exactly the
    /// way it was when it was spoken.</summary>
    public IReadOnlyList<(string Base, string Ruby)>? RubyPairs { get; }
}

/// <summary>
/// Bounded, newest-first backlog. Bounded because it is a convenience, not an archive:
/// the runtime already writes the real record, and an unbounded list in a tray process
/// is a leak with a nice name.
///
/// Index 0 is the live utterance; <see cref="DialogPresenter"/>'s cursor counts back
/// from it, while the readout the user sees counts forward from the oldest.
/// </summary>
public sealed class DialogHistory
{
    private readonly List<HistoryEntry> _entries = new();

    public IReadOnlyList<HistoryEntry> Entries => _entries;

    public HistoryEntry Add(string audioId, string displayText, string? sourceText,
        string? character, string? avatarPath, IReadOnlyList<(string Base, string Ruby)>? rubyPairs)
    {
        var entry = new HistoryEntry(audioId, displayText, sourceText, character, avatarPath,
            rubyPairs);
        _entries.Insert(0, entry);
        if (_entries.Count > DialogTheme.HistoryCapacity)
        {
            _entries.RemoveRange(DialogTheme.HistoryCapacity,
                _entries.Count - DialogTheme.HistoryCapacity);
        }
        return entry;
    }
}
