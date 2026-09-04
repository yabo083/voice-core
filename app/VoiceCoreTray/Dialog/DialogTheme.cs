using Microsoft.UI.Xaml;
using Windows.UI;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// Every colour, metric, face and duration the dialog uses, in one place.
///
/// This is C# and not a XAML <c>ResourceDictionary</c> on purpose: the geometry code
/// needs some of these numbers too (the box width it resizes the window's client area
/// to, the acrylic recipe it hands the backdrop controller), so a XAML-only theme
/// would mean two copies that silently drift. <see cref="DialogWindow"/>'s XAML
/// carries structure only and <c>ApplyTheme()</c> pushes these values onto it once.
/// </summary>
internal static class DialogTheme
{
    // -- placement -----------------------------------------------------------

    /// <summary>Gap between the box's bottom edge and the work area's bottom edge.</summary>
    public const int BottomGapDip = 56;

    // -- box -----------------------------------------------------------------
    //
    // Small and precise on purpose: this is a caption that sits over the user's
    // work, not a window. Every metric here is one step down from the first cut,
    // which read as a dialog box rather than a subtitle plate.

    /// <summary>Upper bound. The box hugs its text below this, so the plate opens
    /// sideways as a line is revealed instead of starting at its final width.</summary>
    public const double BoxMaxWidth = 680;
    /// <summary>Lower bound, so a two-character line is still a plate and not a chip.</summary>
    public const double BoxMinWidth = 220;
    /// <summary>Matches DWM's DWMWCP_ROUND radius. The window is the box and DWM
    /// rounds the window, so a different radius here would show acrylic in the gap
    /// between our corner and the system's.</summary>
    public const double BoxCornerRadius = 8;
    public const double BoxBorderThickness = 1;
    public static readonly Thickness BodyPadding = new(20, 10, 20, 11);

    /// <summary>Top band height. There is no title bar: the band is breathing room
    /// that doubles as the only caption (drag) region, and it holds the controls.</summary>
    public const double TopBandHeight = 22;
    public static readonly Thickness TopBandPadding = new(20, 0, 16, 0);

    public const double ColumnSpacing = 13;
    /// <summary>Gap between the revealed line and the source line under it. Tight on
    /// purpose: the two are one utterance revealed in lockstep, not two paragraphs.</summary>
    public const double RowSpacing = 2;
    public const double AvatarSize = 40;
    /// <summary>Gap between the avatar and the character name under it.</summary>
    public const double NameGap = 5;

    // -- typography ----------------------------------------------------------

    /// <summary>Bundled LXGW WenKai (OFL, assets/fonts) so the dialog reads the same
    /// on any machine instead of borrowing whatever the system happens to have.</summary>
    public const string FontSource =
        "ms-appx:///assets/fonts/LXGWWenKai-Medium.ttf#LXGW WenKai Medium";

    public const double NameSize = 11.5;
    public const double PrimarySize = 18;
    public const double PrimaryLineHeight = 27;
    public const int PrimaryMaxLines = 8;
    /// <summary>Source line: an annotation, so it is small, and its line box is tight
    /// enough that it reads as attached to the line it belongs to.</summary>
    public const double SecondarySize = 11.5;
    public const double SecondaryLineHeight = 15;
    public const int SecondaryMaxLines = 2;
    /// <summary>Gap between the base line and its annotation. Nearly nothing on purpose:
    /// any real spacing turns the pair back into two separate subtitles.</summary>
    public const double AnnotationGap = 1;
    /// <summary>How much wider than its clause an annotation may be before it is shrunk.
    /// Some overhang is correct - ruby hangs over its base - but past about half again the
    /// clause's width it starts colliding with the neighbouring annotations.</summary>
    public const double RubyOverflowRatio = 1.5;
    /// <summary>Floor for that shrink: below this the annotation stops being readable, and
    /// an unreadable annotation is worse than one that overhangs.</summary>
    public const double SecondaryMinSize = 9.5;
    /// <summary>Icon box for the band micro-controls, in DIP.</summary>
    public const double IconSize = 12;
    /// <summary>Padding around each icon that makes up its clickable area. A Path is
    /// only hit-testable where it is filled, so the target is the padded Border around
    /// it: 12 + 2*5 = 22 DIP, which is a comfortable press without looking like a
    /// button.</summary>
    public const double IconHitPadding = 5;
    /// <summary>History position readout in the band ("2 / 12"), same size as a name.</summary>
    public const double BadgeSize = 11;

    // -- band icons ----------------------------------------------------------
    //
    // Fluent 16px geometry from Iconify (fluent:pin-16-regular / -filled,
    // fluent:history-16-regular, fluent:dismiss-16-regular). Kept as path data
    // rather than a font so the three controls stay identical in weight and size
    // whatever fonts the machine has, and stay crisp at any scale.

    public const string PinIcon =
        "M10.059 2.445a1.5 1.5 0 0 0-2.386.354l-2.02 3.79l-2.811.937a.5.5 0 0 0-.196.828L4.793 " +
        "10.5l-2.647 2.647L2 14l.854-.146L5.5 11.207l2.146 2.147a.5.5 0 0 0 .828-.196l.937-2.81l3.779" +
        "-2.024a1.5 1.5 0 0 0 .354-2.38zm-1.504.824a.5.5 0 0 1 .796-.118l3.485 3.498a.5.5 0 0 1-.118" +
        ".794L8.764 9.559a.5.5 0 0 0-.238.283l-.744 2.233l-3.856-3.856l2.232-.744a.5.5 0 0 0 .283-.24z";

    /// <summary>Pinned state: the same pin, filled. A control with no state readout is
    /// a control the user has to guess about, and the outline/filled pair is the Fluent
    /// way to say it without adding a background or a hover effect.</summary>
    public const string PinIconActive =
        "M10.059 2.445a1.5 1.5 0 0 0-2.386.354l-2.02 3.79l-2.811.937a.5.5 0 0 0-.196.828L4.793 " +
        "10.5l-2.647 2.647L2 14l.853-.146L5.5 11.207l2.146 2.147a.5.5 0 0 0 .828-.196l.937-2.81l3.779" +
        "-2.024a1.5 1.5 0 0 0 .354-2.38z";

    public const string LogsIcon =
        "M8 3a5 5 0 1 1-4.98 5.455a.5.5 0 0 0-.996.09A6 6 0 1 0 3.499 4.03V2.5a.5.5 0 1 0-1 0v3A.5.5 " +
        "0 0 0 3 6h3a.5.5 0 1 0 0-1H4a5 5 0 0 1 4-2m0 2.5a.5.5 0 1 0-1 0v3a.5.5 0 0 0 .5.5h2a.5.5 0 1 0 0-1H8z";

    public const string CloseIcon =
        "m2.589 2.716l.057-.07a.5.5 0 0 1 .638-.057l.07.057L8 7.293l4.646-4.647a.5.5 0 0 1 .708.708L8.707 " +
        "8l4.647 4.646a.5.5 0 0 1 .057.638l-.057.07a.5.5 0 0 1-.638.057l-.07-.057L8 8.707l-4.646 4.647a.5.5 " +
        "0 0 1-.708-.708L7.293 8L2.646 3.354a.5.5 0 0 1-.057-.638l.057-.07z";

    /// <summary>
    /// Replay, shown only while browsing the backlog: it is the one action that belongs to
    /// a past line rather than to the dialog.
    ///
    /// A play triangle, not a speaker. Fluent's speaker glyphs carry their sound waves as
    /// separate thin arcs, and at this size a single detached arc beside the cone reads as
    /// a stray mark rather than as sound. The triangle has no detail to lose: two nested
    /// outlines in the same 16x16 viewBox as the others, even-odd filled so the inside is
    /// a hole and the ink weight matches the neighbouring hairline glyphs.
    /// </summary>
    public const string ReplayIcon =
        "F0 M5.2 3.2 L12.4 8 L5.2 12.8 Z M6.28 5.36 L10.24 8 L6.28 10.64 Z";

    // -- colours -------------------------------------------------------------
    //
    // Readable over arbitrary desktop content. The stroke is a gradient too: a
    // uniform light hairline reads as a drawn-on white line, which is the single
    // thing that makes a dialog box look cheap. Bright at the top edge where a
    // light would catch it, almost nothing at the bottom.

    /// <summary>Sheen painted ON TOP of the acrylic, not the plate itself: a little
    /// vertical shading so the surface has a direction and the text near the bottom
    /// keeps its contrast over a busy window behind the dialog.</summary>
    public static readonly Color PlateSheenTop = Color.FromArgb(0x00, 0x00, 0x00, 0x00);
    public static readonly Color PlateSheenBottom = Color.FromArgb(0x2E, 0x05, 0x06, 0x0A);
    public static readonly Color StrokeTop = Color.FromArgb(0x24, 0xFF, 0xFF, 0xFF);
    public static readonly Color StrokeBottom = Color.FromArgb(0x08, 0xFF, 0xFF, 0xFF);
    /// <summary>Inner top highlight: one hairline of light just inside the stroke,
    /// which is what gives an edge thickness instead of a printed outline.</summary>
    public static readonly Color InnerHighlight = Color.FromArgb(0x14, 0xFF, 0xFF, 0xFF);
    /// <summary>Speaker name. The identity violet, which is what separates who is
    /// talking from what was said without adding a second type size.</summary>
    public static readonly Color NameInk = Color.FromArgb(0xFF, 0xA4, 0x8B, 0xFF);
    /// <summary>Body ink. Hueless near-white, deliberately NOT the shell's dimmer body ink:
    /// this line sits over video and game frames, so its luminance is legibility, not style.</summary>
    public static readonly Color PrimaryInk = Color.FromArgb(0xFF, 0xF2, 0xF2, 0xF2);
    public static readonly Color SecondaryInk = Color.FromArgb(0x9E, 0xFF, 0xFF, 0xFF);
    public static readonly Color ActionInk = Color.FromArgb(0x8A, 0xFF, 0xFF, 0xFF);
    /// <summary>An engaged control (pin held down). Same violet as the name, so
    /// "on" reads as lit rather than as a different colour scheme.</summary>
    public static readonly Color ActionInkActive = Color.FromArgb(0xFF, 0xA4, 0x8B, 0xFF);
    /// <summary>Waiting indicator. The identity violet lifted to a near-white tint rather than
    /// the saturated accent: a vivid glyph blinking on a dark plate reads as a status LED.</summary>
    public static readonly Color IndicatorInk = Color.FromArgb(0xFF, 0xC4, 0xB4, 0xFF);
    /// <summary>Micro-controls sit at one fixed opacity: present enough to find, quiet
    /// enough that a caption on the desktop still reads as text. Deliberately not
    /// hover-driven - see the note on the band markup.</summary>
    public const double ControlsOpacity = 0.62;

    // -- backdrop ------------------------------------------------------------
    //
    // The plate is system acrylic, not paint: the window is the box, so DesktopAcrylic
    // frosts whatever is behind it and DWM rounds and shadows it. These four values
    // are the whole plate recipe.

    /// <summary>Acrylic tint. Hueless graphite, the same surface the shell uses: the tint
    /// sits over the blurred desktop, so a hue here casts over whatever is behind the box.</summary>
    public static readonly Color AcrylicTint = Color.FromArgb(0xFF, 0x16, 0x16, 0x16);
    /// <summary>How much of the tint colour, versus the blurred content behind.</summary>
    public const float AcrylicTintOpacity = 0.55f;
    /// <summary>Luminosity floor. High enough that white text keeps its contrast over
    /// a bright window behind the dialog.</summary>
    public const float AcrylicLuminosityOpacity = 0.90f;
    /// <summary>Used when acrylic is unavailable (battery saver, transparency off).</summary>
    public static readonly Color AcrylicFallback = Color.FromArgb(0xF2, 0x1C, 0x1C, 0x1C);
    public static readonly Color CountdownTrackInk = Color.FromArgb(0x1A, 0xFF, 0xFF, 0xFF);
    public static readonly Color CountdownInk = Color.FromArgb(0xD9, 0x8B, 0x6C, 0xEF);
    /// <summary>Countdown bar while the pointer freezes it, so the freeze is visible. The
    /// shell's warn amber: a held countdown is a state to notice, not a second accent.</summary>
    public static readonly Color CountdownFrozenInk = Color.FromArgb(0xFF, 0xE0, 0xAC, 0x00);

    public const double CountdownHeight = 2;
    /// <summary>Waiting triangle, drawn as a Path so it is crisp at any scale
    /// instead of borrowing a glyph from whatever font is around.</summary>
    public const double IndicatorWidth = 9;
    public const double IndicatorHeight = 6;
    /// <summary>Vertical travel of the indicator's bob, in DIP.</summary>
    public const double IndicatorBob = 3;

    // -- motion --------------------------------------------------------------

    /// <summary>Typewriter tick. One reveal per compositor frame is plenty.</summary>
    public static readonly TimeSpan TypeTick = TimeSpan.FromMilliseconds(16);

    // -- reveal presets ------------------------------------------------------
    //
    // Alternatives to the typewriter, for when the point is the finished sentence rather
    // than the act of speaking it. Both start from a COMPLETE layout - every segment and
    // annotation measured and placed - so nothing moves while they play.

    /// <summary>Per-segment fade: how long one segment takes to come up.</summary>
    public static readonly TimeSpan FadeInSegment = TimeSpan.FromMilliseconds(220);
    /// <summary>Gap between one segment starting and the next, in `fade`. A clause-sized
    /// pause, so the line arrives in phrases rather than all at once.</summary>
    public static readonly TimeSpan FadeStagger = TimeSpan.FromMilliseconds(100);
    /// <summary>Total travel of the `sweep` preset across the whole block, and the width of
    /// its soft edge in time. Each segment starts in proportion to its x position, so the
    /// sweep keeps one speed regardless of how many segments a line has.</summary>
    public static readonly TimeSpan SweepTravel = TimeSpan.FromMilliseconds(420);
    public static readonly TimeSpan SweepFeather = TimeSpan.FromMilliseconds(200);

    /// <summary>
    /// Box growth (typewriter only: the other presets start at the final size). The window
    /// chases the size the text needs instead of jumping to it, which is what makes the plate
    /// look like it is opening as the line arrives. Exponential follow, so it is frame-rate
    /// independent and has no fixed duration to fight with the typewriter: each frame closes
    /// this fraction of the gap.
    /// </summary>
    public const double GrowFollowPerFrame = 0.30;

    /// <summary>
    /// Growth frame interval. NOT 16 ms: each frame ends in an <c>AppWindow.MoveAndResize</c>,
    /// measured at ~5 ms of UI-thread time (SetWindowPos plus a DWM recomposition of an
    /// acrylic-backed window), and the tray's low-level mouse hook shares that thread. 24 ms
    /// keeps the motion smooth while leaving the thread mostly idle - see
    /// <c>logs/dialog.jsonl</c> for what it actually costs.
    /// </summary>
    public static readonly TimeSpan GrowTick = TimeSpan.FromMilliseconds(24);

    /// <summary>Resize quantum in physical pixels while animating, and the gap at which
    /// the motion snaps to its target. Measured: one <c>MoveAndResize</c> of this window
    /// costs 4-8 ms of UI thread, so every extra distinct size is real latency, while a
    /// 4 px step of an opening plate is invisible. Together these cut the resizes of a
    /// 43-character line from 44 to roughly 20 (<c>logs/dialog.jsonl</c>).</summary>
    public const int GrowQuantumPx = 4;
    public const double GrowSnapPx = 3;
    public static readonly TimeSpan FadeIn = TimeSpan.FromMilliseconds(140);
    public static readonly TimeSpan FadeOut = TimeSpan.FromMilliseconds(260);
    public static readonly TimeSpan IndicatorBlink = TimeSpan.FromMilliseconds(1400);
    public const double IndicatorDimOpacity = 0.28;

    // -- pacing --------------------------------------------------------------

    /// <summary>Reveal budget per character when no audio sets the pace.</summary>
    public const double TypeSecondsPerChar = 0.045;
    public const double TypeMinSeconds = 0.35;
    /// <summary>Upper bound so a 400-character line does not type for half a minute.</summary>
    public const double TypeMaxSeconds = 4.5;
    /// <summary>Fraction of the spoken audio the reveal is stretched over: the line
    /// lands just before the voice stops, which is the Galgame feel.</summary>
    public const double TypeAudioRatio = 0.85;

    /// <summary>Countdown length used when nothing else specifies one.</summary>
    public const double DefaultDwellSeconds = 6.0;

    /// <summary>
    /// How many utterances may wait behind the one on screen. A batch arrives faster than
    /// it can be spoken, and a line's audio and text must finish together, so the excess
    /// waits here; past this depth the OLDEST waiting line is dropped, because a caption
    /// running minutes behind the voice is worse than one that skipped.
    /// </summary>
    public const int QueueCapacity = 8;
    /// <summary>Bounded backlog kept for in-place backtracking (wheel up over the box).</summary>
    public const int HistoryCapacity = 50;
}
