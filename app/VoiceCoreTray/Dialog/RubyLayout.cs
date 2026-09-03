using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// Lays out an utterance together with the line that was actually spoken.
///
/// THE HONEST PART FIRST. Chinese and Japanese do not line up positionally: the verb sits
/// mid-sentence in one and at the end of the other, and a translation freely merges, splits
/// or reorders clauses. Any layout that cuts the two strings by character ratio, or pairs
/// clause 3 with clause 3 regardless, is asserting a correspondence that is not there - and
/// a reader who trusts it is being misled by the typography.
///
/// So the alignment is DATA, not a guess. Whoever called <c>speak</c> produced both strings
/// and knows which fragment means which; it sends <c>rubyPairs</c> and this class renders
/// them, one cell per pair. That is the path to use.
///
/// When no pairs arrive (a human at the CLI, an older caller), the layout only claims what
/// it can prove:
///   * <b>Clause mode</b> - both sides split into the SAME number of clauses. Then clause i
///     really does correspond to clause i, and each clause of the source line is centred on
///     the clause it belongs to.
///   * <b>Gloss mode</b> - the counts differ. No positional claim is made at all: the whole
///     source line is one run attached to the whole utterance. It reads as "this is what was
///     said", which is true, instead of "this word means that word", which would not be.
///
/// Every mode is revealed in lockstep by fraction, so the pair always arrives as one
/// utterance in TIME even when it cannot be paired in SPACE.
/// </summary>
internal static class RubyLayout
{
    /// <summary>One segment of the utterance and the annotation that belongs to it.
    ///   * <paramref name="Width"/> is the BASE's width, never the annotation's. An
    ///     annotation longer than its base overhangs sideways (real ruby does too) instead
    ///     of widening the cell, because a widened cell pushes the neighbouring segments
    ///     apart and the line stops reading as one sentence.
    ///   * <paramref name="RubySize"/> is that annotation's font size, reduced when it would
    ///     overhang far enough to collide with its neighbours.</summary>
    internal readonly record struct Cell(
        string Base, string Ruby, double Width, double RubyWidth, double RubySize);

    /// <summary>Lines of clause cells, plus the whole-utterance gloss when the two sides
    /// could not be paired. Exactly one of the two carries the annotation.</summary>
    internal sealed record Layout(List<List<Cell>> Lines, string Gloss)
    {
        /// <summary>True when the annotation is attached clause by clause.</summary>
        public bool Paired => Gloss.Length == 0 && Lines.Any(l => l.Any(c => c.Ruby.Length > 0));
    }

    /// <summary>Clause terminators, kept with the clause they end. Both scripts use most of
    /// these; ASCII punctuation is included because display text is often mixed.</summary>
    private const string Terminators = "，。！？；、,.!?;…";

    internal static Layout Build(string primary, string? annotation,
        IReadOnlyList<(string Base, string Ruby)>? supplied, double maxWidth,
        TextBlock baseProbe, TextBlock rubyProbe, int maxLines)
    {
        if (primary.Length == 0) return new Layout(new List<List<Cell>>(), string.Empty);

        // The caller's own alignment wins over anything guessable here: it knows which
        // fragment means which, and this class does not. But it MUST cover the utterance:
        // the reveal is paced by primary's length and Fill spreads that prefix over these
        // cells, so bases that do not add up to it leave the tail permanently unrevealed -
        // silently, on every utterance. Fall through to clause pairing rather than display a
        // sentence that can never finish.
        if (supplied is { Count: > 0 })
        {
            int covered = 0;
            foreach (var pair in supplied) covered += pair.Base.Length;
            if (covered == primary.Length)
            {
                var given = supplied
                    .Select(p => Sized(new Cell(p.Base, Annotation(p.Ruby), 0, 0, 0), baseProbe, rubyProbe))
                    .ToList();
                return new Layout(Wrap(given, maxWidth, baseProbe, rubyProbe, maxLines), string.Empty);
            }
        }

        var baseClauses = Clauses(primary);
        var rubyClauses = Clauses(annotation ?? string.Empty);

        bool paired = rubyClauses.Count > 0 && rubyClauses.Count == baseClauses.Count;
        var cells = new List<Cell>(baseClauses.Count);
        for (int i = 0; i < baseClauses.Count; i++)
        {
            cells.Add(Sized(
                new Cell(baseClauses[i], paired ? Annotation(rubyClauses[i]) : string.Empty, 0, 0, 0),
                baseProbe, rubyProbe));
        }

        return new Layout(Wrap(cells, maxWidth, baseProbe, rubyProbe, maxLines),
            paired ? string.Empty : annotation ?? string.Empty);
    }

    /// <summary>
    /// An annotation carries WORDS, not punctuation. The base line already shows where a
    /// clause ends, so a lone 「、」 under a 「，」 adds nothing and at this size mostly reads
    /// as dirt on the plate. Trimmed from both ends; a fragment that was only punctuation
    /// annotates nothing and disappears, which also means its cell reserves no ruby row.
    /// </summary>
    private static string Annotation(string ruby) =>
        ruby.Trim().Trim(Terminators.ToCharArray()).Trim();

    /// <summary>Split into clauses, keeping the terminator on the clause it closes.</summary>
    private static List<string> Clauses(string text)
    {
        var result = new List<string>();
        int start = 0;
        for (int i = 0; i < text.Length; i++)
        {
            if (Terminators.IndexOf(text[i]) < 0) continue;
            // Runs of punctuation ("...!?") belong to the same clause.
            while (i + 1 < text.Length && Terminators.IndexOf(text[i + 1]) >= 0) i++;
            result.Add(text[start..(i + 1)]);
            start = i + 1;
        }
        if (start < text.Length) result.Add(text[start..]);
        return result;
    }

    /// <summary>Greedy pack of clause cells into lines, breaking inside a clause only when it
    /// does not fit a line by itself.</summary>
    private static List<List<Cell>> Wrap(List<Cell> cells, double maxWidth,
        TextBlock baseProbe, TextBlock rubyProbe, int maxLines)
    {
        var lines = new List<List<Cell>>();
        var current = new List<Cell>();
        double used = 0;

        foreach (var cell in cells)
        {
            foreach (var piece in Fit(cell, maxWidth, baseProbe, rubyProbe, maxLines + 1))
            {
                if (used > 0 && used + piece.Width > maxWidth)
                {
                    lines.Add(current);
                    current = new List<Cell>();
                    used = 0;

                    if (lines.Count == maxLines) return Truncate(lines);
                }
                current.Add(piece);
                used += piece.Width;
            }
        }

        if (current.Count > 0) lines.Add(current);
        return lines;
    }

    /// <summary>
    /// A clause wider than the whole line has to break inside itself. The break is by
    /// character, which is correct for CJK; in clause mode the annotation goes with the FIRST
    /// piece rather than being cut, because half a Japanese clause under half a Chinese one
    /// is exactly the false correspondence this file exists to avoid.
    /// </summary>
    private static List<Cell> Fit(Cell cell, double maxWidth, TextBlock baseProbe, TextBlock rubyProbe,
        int maxPieces)
    {
        var pieces = new List<Cell>();
        if (cell.Width <= maxWidth || cell.Base.Length <= 1)
        {
            pieces.Add(cell);
            return pieces;
        }

        int start = 0;
        // One piece is one line, so stop at the line budget: every further piece is thrown
        // away by Truncate, and producing them is quadratic in the clause's length (each one
        // measures the whole remainder, then binary-searches inside it). A pasted 10k-character
        // clause with no punctuation in it would otherwise freeze the tray - synchronously, on
        // the thread that owns the desktop mouse hook, before the audio starts.
        while (start < cell.Base.Length && pieces.Count < maxPieces)
        {
            int fits = LongestFit(cell.Base, start, maxWidth, baseProbe);
            var piece = new Cell(cell.Base.Substring(start, fits),
                start == 0 ? cell.Ruby : string.Empty, 0, 0, 0);
            pieces.Add(Sized(piece, baseProbe, rubyProbe));
            start += fits;
        }
        return pieces;
    }

    private static int LongestFit(string text, int start, double maxWidth, TextBlock probe)
    {
        int remaining = text.Length - start;
        if (Width(text.Substring(start, remaining), probe) <= maxWidth) return remaining;

        int low = 1, high = remaining;
        while (low < high)
        {
            int mid = (low + high + 1) / 2;
            if (Width(text.Substring(start, mid), probe) <= maxWidth) low = mid;
            else high = mid - 1;
        }
        return Math.Max(1, low);
    }

    /// <summary>
    /// Fill in the widths and the annotation's font size.
    ///
    /// The cell's width is the base's, so an over-long annotation overhangs instead of
    /// spreading the utterance - but unbounded overhang collides with the neighbouring
    /// annotations (4 hanzi under 7 kana is enough to reach the portrait). Past
    /// <see cref="DialogTheme.RubyOverflowRatio"/> the annotation is therefore scaled down
    /// until it fits that bound or hits <see cref="DialogTheme.SecondaryMinSize"/>, whichever
    /// comes first.
    /// </summary>
    private static Cell Sized(Cell cell, TextBlock baseProbe, TextBlock rubyProbe)
    {
        double baseWidth = Width(cell.Base, baseProbe);
        double size = DialogTheme.SecondarySize;
        rubyProbe.FontSize = size;
        double rubyWidth = Width(cell.Ruby, rubyProbe);

        double allowed = baseWidth * DialogTheme.RubyOverflowRatio;
        if (rubyWidth > allowed && allowed > 0)
        {
            size = Math.Max(DialogTheme.SecondaryMinSize, size * allowed / rubyWidth);
            rubyProbe.FontSize = size;
            rubyWidth = Width(cell.Ruby, rubyProbe);
            rubyProbe.FontSize = DialogTheme.SecondarySize;
        }

        return cell with { Width = baseWidth, RubyWidth = rubyWidth, RubySize = size };
    }

    /// <summary>Mark the loss on the last kept line, the way a trimmed TextBlock would.</summary>
    private static List<List<Cell>> Truncate(List<List<Cell>> lines)
    {
        var last = lines[^1];
        if (last.Count > 0)
        {
            var cell = last[^1];
            last[^1] = cell with { Base = cell.Base.TrimEnd() + "…" };
        }
        return lines;
    }

    /// <summary>
    /// Measure one string with a probe. <c>InvalidateMeasure</c> first, always: a TextBlock
    /// whose measure is not dirty hands back its PREVIOUS DesiredSize, and this method
    /// changes both the text and (for annotations) the font size between calls - which is
    /// exactly how a stale width from an earlier utterance ends up deciding this one's
    /// layout.
    /// </summary>
    private static double Width(string text, TextBlock probe)
    {
        if (text.Length == 0) return 0;
        probe.Text = text;
        probe.InvalidateMeasure();
        probe.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        return probe.DesiredSize.Width;
    }
}
