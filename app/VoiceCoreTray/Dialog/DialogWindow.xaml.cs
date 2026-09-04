using Microsoft.UI;
using Microsoft.UI.Composition;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Microsoft.UI.Xaml.Media.Imaging;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Text;
using VoiceCoreTray.Services;
using Windows.Foundation;
using Windows.Graphics;
using WinUIEx.Messaging;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// The dialog's view container: an always-on-top, draggable Galgame dialog box in its
/// own window (so it never fights the tray window's Mica backdrop). It renders and
/// reports gestures; the lifecycle lives in <see cref="DialogPresenter"/>.
///
/// The window IS the box, and DWM decorates it (ADR-0009, superseding the v1 fixed
/// canvas of ADR-0008):
///   * Every relayout resizes the client area to the laid-out box and moves it in the
///     same <c>MoveAndResize</c> call, so the box never resizes in two visible steps.
///     Text is still never measured in code: XAML lays the box out, this copies the
///     result. The ghost line in <see cref="BeginUtterance"/> fixes the final height
///     up front, so an utterance resizes the window once, not once per character.
///   * The plate, its rounded corners and its shadow are all system-drawn:
///     <see cref="DesktopAcrylicController"/> for the frosted plate,
///     <c>DWMWA_WINDOW_CORNER_PREFERENCE</c> for the corners, and DWM's own window
///     shadow. There is no transparency key, no <c>SetWindowRgn</c> clip and no
///     hand-drawn shadow left to go wrong.
///   * Two things are load-bearing and non-obvious, both measured:
///       - the presenter MUST keep its border (<c>SetBorderAndTitleBar(true, false)</c>).
///         A frameless WinUI window gets neither DWM rounding nor a DWM shadow.
///       - the acrylic MUST be driven through a controller with
///         <c>IsInputActive = true</c> pinned on. <c>Window.SystemBackdrop</c> ties
///         sampling to input activation, and this window is WS_EX_NOACTIVATE, so the
///         stock path renders the flat fallback tint forever.
///   * Placement is anchored on the box's bottom-center, never on the window origin:
///     the box grows upward, keeps the position the user dragged it to across
///     utterances of any length, and is always clamped into the work area.
///   * Dragging returns HTCAPTION from <c>WM_NCHITTEST</c> over the top band: the OS
///     move loop owns the drag, so it is pixel-exact and lag-free. Combined with
///     WS_EX_NOACTIVATE (never take focus) a press starts the drag immediately
///     instead of being swallowed by window activation.
///   * Region and hit-test rects are CLIENT space, and a framed window's client area
///     is inset from its window rect, so <see cref="ClientOffset"/> is what converts
///     between the two. Getting this wrong moves the box by the frame width.
/// </summary>
public sealed partial class DialogWindow : Window
{
    private const int GWL_EXSTYLE = -20;
    private const int WS_EX_NOACTIVATE = 0x0800_0000;
    private const nint WS_EX_TOPMOST = 0x0000_0008;

    private const uint WM_NCHITTEST = 0x0084;
    private const uint WM_NCLBUTTONDBLCLK = 0x00A3;
    private const uint WM_NCRBUTTONDOWN = 0x00A4;
    private const uint WM_NCRBUTTONUP = 0x00A5;
    private const uint WM_EXITSIZEMOVE = 0x0232;
    private const uint WM_DPICHANGED = 0x02E0;
    private const uint WM_ERASEBKGND = 0x0014;

    private const nint HTCLIENT = 1;
    private const nint HTCAPTION = 2;

    /// <summary>DWMWA_WINDOW_CORNER_PREFERENCE / DWMWCP_ROUND: the system rounds the
    /// window, so nothing here has to clip a region to get rounded corners.</summary>
    private const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
    private const int DWMWCP_ROUND = 2;

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern nint GetWindowLongPtr(nint hWnd, int index);
    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern nint SetWindowLongPtr(nint hWnd, int index, nint value);
    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint hWnd);
    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(nint hWnd, out RECT rect);
    [DllImport("user32.dll")]
    private static extern bool ClientToScreen(nint hWnd, ref POINTL point);
    [DllImport("user32.dll")]
    private static extern bool GetClientRect(nint hWnd, out RECT rect);
    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINTL point);
    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(nint hWnd, int attribute, ref int value, int size);
    [DllImport("user32.dll")]
    private static extern int FillRect(nint hdc, ref RECT rect, nint brush);
    [DllImport("gdi32.dll")]
    private static extern nint CreateSolidBrush(uint color);
    [DllImport("gdi32.dll")]
    private static extern bool DeleteObject(nint handle);

    private struct RECT { public int Left, Top, Right, Bottom; }
    private struct POINTL { public int X, Y; }

    private readonly string _dataDir;
    private readonly DialogMetrics _metrics;
    private readonly AppConfig.DialogSection _options;
    /// <summary>Current utterance's layout - clause cells grouped into wrapped lines, or a
    /// single gloss when the two languages could not be paired - plus the blocks rendering
    /// it. Rebuilt per utterance, filled per reveal.</summary>
    private RubyLayout.Layout _layout = new(new List<List<RubyLayout.Cell>>(), string.Empty);
    private readonly List<(TextBlock Base, TextBlock? Ruby, RubyLayout.Cell Cell)> _cells = new();
    /// <summary>The face the probes measured with: every run MUST use it or the wrap
    /// points computed against the probe are fiction.</summary>
    private FontFamily _face = new("Segoe UI");
    private TextBlock? _gloss;
    /// <summary>Each animatable piece of the current layout with its x offset and width
    /// inside the text block: what the `fade` and `sweep` presets animate.</summary>
    private readonly List<(UIElement Element, double X, double Width)> _segments = new();
    private Storyboard? _reveal;
    /// <summary>Measures how late each growth frame actually ran: the UI-thread stall.</summary>
    private readonly System.Diagnostics.Stopwatch _growClock = new();
    private readonly WindowMessageMonitor _messages;
    private readonly InputNonClientPointerSource _nonClient;
    private readonly nint _hwnd;
    private readonly SolidColorBrush _countdownInk;
    private readonly SolidColorBrush _countdownFrozenInk;

    /// <summary>Box bounds in CLIENT physical pixels. The client area IS the box, so
    /// this is (0, 0, client width, client height) - kept as a rect because the hit
    /// tests and the anchor maths read it as one.</summary>
    private RectInt32 _boxPx;
    /// <summary>Top band (drag handle) bounds in CLIENT physical pixels.</summary>
    private RectInt32 _bandPx;
    /// <summary>Micro-control cluster inside the band, padded, in CLIENT physical
    /// pixels: the one part of the band that must NOT answer as caption.</summary>
    private RectInt32 _controlsPx;
    /// <summary>Box bottom-center in screen physical pixels; null = default placement.</summary>
    private PointInt32? _anchor;
    private Storyboard? _fade;
    private Storyboard? _blink;
    private Storyboard? _countdown;
    /// <summary>Box size the text currently needs, and the animated values chasing it,
    /// all in physical pixels. The gap between them IS the growth motion, and it is two
    /// dimensional: a short line is a narrow plate, a wrapped one is a tall plate.</summary>
    private double _targetWidth;
    private double _targetHeight;
    private double _appliedWidth;
    private double _appliedHeight;
    private DispatcherQueueTimer? _grow;
    private DesktopAcrylicController? _acrylic;
    private SystemBackdropConfiguration? _backdropConfig;
    /// <summary>GDI brush for <c>WM_ERASEBKGND</c>. Created once and kept: the resize path
    /// hits it dozens of times while a plate opens.</summary>
    private nint _eraseBrush;
    /// <summary>Re-entrancy guard: <see cref="SyncBoxBounds"/> resizes the window and
    /// forces layout, both of which can call back into it.</summary>
    private bool _inSync;
    private bool _shown;

    internal DialogWindow(string dataDir, DialogMetrics metrics)
    {
        InitializeComponent();
        _dataDir = dataDir;
        _metrics = metrics;
        _options = AppConfig.Load(dataDir).Dialog;

        Title = "voice-core dialog";

        var presenter = (OverlappedPresenter)AppWindow.Presenter;
        presenter.IsAlwaysOnTop = true;
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        // Border kept on purpose: a frameless WinUI window gets neither DWM rounding
        // nor a DWM shadow. Measured, not assumed - see the class comment.
        presenter.SetBorderAndTitleBar(hasBorder: true, hasTitleBar: false);
        AppWindow.IsShownInSwitchers = false;

        _hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        // Never take focus. An inactive window eats the first click for activation,
        // which is what delayed the start of the drag.
        SetWindowLongPtr(_hwnd, GWL_EXSTYLE, GetWindowLongPtr(_hwnd, GWL_EXSTYLE) | WS_EX_NOACTIVATE);

        int corner = DWMWCP_ROUND;
        DwmSetWindowAttribute(_hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref corner, sizeof(int));
        EnableAcrylic();

        _countdownInk = new SolidColorBrush(DialogTheme.CountdownInk);
        _countdownFrozenInk = new SolidColorBrush(DialogTheme.CountdownFrozenInk);
        ApplyTheme();

        _messages = new WindowMessageMonitor(_hwnd);
        _messages.WindowMessageReceived += OnWindowMessage;
        Closed += (_, _) =>
        {
            _messages.Dispose();
            _acrylic?.Dispose();
            if (_eraseBrush != 0) DeleteObject(_eraseBrush);
        };

        // The XAML island is a child HWND that owns pointer input over the client area, so
        // the parent's WM_NCHITTEST never sees a press on the box. Declaring the box a
        // caption region is the supported way to get the OS move loop (and with it native
        // drag feel) for content inside the island.
        _nonClient = InputNonClientPointerSource.GetForWindowId(AppWindow.Id);

        // Any box relayout (new text, DPI change) resizes and re-anchors the window.
        DialogBox.SizeChanged += (_, _) => SyncBoxBounds();

        // Pointer contract, deliberately blunt: the WHOLE box drags with the left button
        // (caption region + HTCAPTION), the right button dismisses (WM_NCRBUTTONUP), and
        // the only left-clickable things are the band's icons, which are cut out of the
        // caption. Nothing in the body raises a XAML pointer event any more - a caption
        // region hands its input to the move loop, so a body handler would be dead code
        // pretending to be a feature.
        //
        // Contextual micro-controls are wired on the Border around each icon, not the
        // icon: a Path is hit-testable only where its geometry is filled, and these are
        // thin strokes - pressing the glyph itself means hitting a 1 px diagonal.
        // SyncRegions punches a Passthrough hole over the cluster and WM_NCHITTEST answers
        // HTCLIENT there; without either, the move loop eats the press and the icons look
        // purely decorative.
        //
        // No hover state on purpose: the band is a caption region with a Passthrough hole,
        // so pointer enter/exit fire every time the pointer crosses a gap between icons or
        // the hole's edge, and anything that reacts to them flickers.
        WireBandControl(ReplayHit, () => ReplayRequested?.Invoke());
        WireBandControl(LogsHit, () => HistoryRequested?.Invoke());
        WireBandControl(CloseHit, () => CloseRequested?.Invoke());

        if (SubtitleOptions.LoadAnchor(_dataDir) is (int ax, int ay))
            _anchor = new PointInt32(ax, ay);

        ApplyBoxWidth();
    }

    /// <summary>The band's 历史 control: step into the backlog, or back out of it.</summary>
    public event Action? HistoryRequested;

    /// <summary>The band's 重播 control, only present while browsing: replay that line.</summary>
    public event Action? ReplayRequested;

    /// <summary>The band's 关闭 control, and a right-click anywhere: hide the dialog.</summary>
    public event Action? CloseRequested;

    /// <summary>
    /// One band control: press fires, nothing else. Press rather than Tapped — the
    /// window never activates (WS_EX_NOACTIVATE) and Tapped on a non-activating
    /// window is unreliable. The target is the icon's Border, whose transparent
    /// background is the actual hit area.
    /// </summary>
    private void WireBandControl(Border control, Action action)
    {
        control.Background = new SolidColorBrush(Colors.Transparent);
        control.Padding = new Thickness(DialogTheme.IconHitPadding);
        control.PointerPressed += (_, e) =>
        {
            e.Handled = true;
            action();
        };
    }

    /// <summary>
    /// Fill one band icon and put it in its cell at ONE shared scale.
    ///
    /// The paths are all authored in Fluent's 16x16 viewBox, so scaling every one by
    /// IconSize/16 preserves both the designer's optical sizing and the padding they
    /// left around the ink. Scaling each path's own tight bounds to the cell instead
    /// (Stretch=Uniform) makes a glyph that fills its box small and one that does not
    /// large, which is the mismatch this replaces.
    /// </summary>
    private static void StyleIcon(Microsoft.UI.Xaml.Shapes.Path icon, Canvas cell, string data)
    {
        const double viewBox = 16;
        double scale = DialogTheme.IconSize / viewBox;

        cell.Width = DialogTheme.IconSize;
        cell.Height = DialogTheme.IconSize;
        icon.Fill = new SolidColorBrush(DialogTheme.ActionInk);
        icon.RenderTransform = new ScaleTransform { ScaleX = scale, ScaleY = scale };
        icon.Data = (Geometry)Microsoft.UI.Xaml.Markup.XamlBindingHelper
            .ConvertValue(typeof(Geometry), data);
    }

    /// <summary>
    /// Swap the band's controls for the mode the dialog is in. Live: 历史 + 关闭. Browsing:
    /// 重播 (replay this line) + 关闭. The 历史 control goes because in that state it would
    /// be the one control whose meaning changes, and replay only exists there because it
    /// acts on a past line.
    /// </summary>
    public void SetBrowsing(bool browsing)
    {
        var logs = browsing ? Visibility.Collapsed : Visibility.Visible;
        var replay = browsing ? Visibility.Visible : Visibility.Collapsed;
        if (LogsHit.Visibility == logs && ReplayHit.Visibility == replay) return;

        LogsHit.Visibility = logs;
        ReplayHit.Visibility = replay;
        // The cluster changed width, so the caption hole has to be re-punched.
        RootGrid.UpdateLayout();
        SyncBoxBounds(snap: true);
    }

    public bool IsShown => _shown;

    // -- theme ---------------------------------------------------------------

    /// <summary>Push every value from <see cref="DialogTheme"/> onto the structure the
    /// XAML declares. One place, one direction.</summary>
    private void ApplyTheme()
    {
        var face = new FontFamily(DialogTheme.FontSource);

        // Width band comes from ApplyBoxWidth (it needs the work area); inside it the box
        // hugs its text, which is what the opening motion animates.
        // The plate is the window's acrylic; this is only a sheen over it. A shadow
        // margin would be wrong here: DWM draws the shadow OUTSIDE the window.
        DialogBox.Background = VerticalGradient(DialogTheme.PlateSheenTop, DialogTheme.PlateSheenBottom);
        DialogBox.BorderBrush = VerticalGradient(DialogTheme.StrokeTop, DialogTheme.StrokeBottom);
        DialogBox.BorderThickness = new Thickness(DialogTheme.BoxBorderThickness);
        DialogBox.CornerRadius = new CornerRadius(DialogTheme.BoxCornerRadius);

        // Top-only inner hairline. A full inner ring would double the outline and
        // land back at the flat white line this replaces.
        InnerEdge.BorderBrush = new SolidColorBrush(DialogTheme.InnerHighlight);
        InnerEdge.BorderThickness = new Thickness(0, DialogTheme.BoxBorderThickness, 0, 0);
        InnerEdge.CornerRadius = new CornerRadius(
            DialogTheme.BoxCornerRadius - DialogTheme.BoxBorderThickness,
            DialogTheme.BoxCornerRadius - DialogTheme.BoxBorderThickness, 0, 0);
        InnerEdge.VerticalAlignment = VerticalAlignment.Stretch;

        // No title bar: the band has no fill and no divider, so the plate reads as one
        // surface and the micro-controls sit in the quiet space at its top edge.
        TopBand.Height = DialogTheme.TopBandHeight;
        TopBand.Padding = DialogTheme.TopBandPadding;

        // Two lines allowed: a long character name wraps under the portrait instead of
        // stretching the column.
        StyleText(NameText, face, DialogTheme.NameSize, 0, 2, DialogTheme.NameInk);
        StyleText(HistoryBadge, face, DialogTheme.BadgeSize, 0, 1, DialogTheme.ActionInk);

        // Vector icons, not font glyphs: identical weight and size on any machine, and
        // all of them at one scale in one cell size.
        StyleIcon(ReplayButton, ReplayBox, DialogTheme.ReplayIcon);
        StyleIcon(LogsButton, LogsBox, DialogTheme.LogsIcon);
        StyleIcon(CloseButton, CloseBox, DialogTheme.CloseIcon);
        BandControls.Opacity = DialogTheme.ControlsOpacity;

        Body.Padding = DialogTheme.BodyPadding;
        // A null Background is not hit-testable. The body raises no pointer events any
        // more (it is caption), but a null brush also breaks the island's own hit test.
        Body.Background = new SolidColorBrush(Colors.Transparent);
        Body.ColumnSpacing = DialogTheme.ColumnSpacing;
        TextColumn.Spacing = DialogTheme.RowSpacing;

        AvatarFrame.Width = DialogTheme.AvatarSize;
        AvatarFrame.Height = DialogTheme.AvatarSize;
        AvatarFrame.CornerRadius = new CornerRadius(DialogTheme.AvatarSize / 2);
        // The name belongs to the portrait, so it sits under it and never wider than
        // it: a long name must wrap rather than widen the character column.
        NameText.Margin = new Thickness(0, DialogTheme.NameGap, 0, 0);
        NameText.MaxWidth = DialogTheme.AvatarSize + DialogTheme.ColumnSpacing;

        // The line rows are generated per utterance (BuildLines), so what is styled here is
        // the face they are built with and the probe their wrapping is measured against -
        // the probe MUST match the base line's font exactly or the wrap points are fiction.
        _face = face;
        StyleText(WrapProbe, face, DialogTheme.PrimarySize, DialogTheme.PrimaryLineHeight,
            1, DialogTheme.PrimaryInk);
        WrapProbe.TextWrapping = TextWrapping.NoWrap;
        WrapProbe.TextTrimming = TextTrimming.None;
        // Same rule for the annotation probe. RubyLayout measures every Cell.RubyWidth with
        // it, and those runs render in `face` (NewRuby): measuring them in TextBlock's
        // default face instead makes the centring offset, the overhang clamp and the
        // shrink-to-fit decision all fire on a width the run does not have.
        StyleText(RubyProbe, face, DialogTheme.SecondarySize, DialogTheme.SecondaryLineHeight,
            1, DialogTheme.SecondaryInk);
        RubyProbe.TextWrapping = TextWrapping.NoWrap;
        RubyProbe.TextTrimming = TextTrimming.None;
        WaitingIndicator.Width = DialogTheme.IndicatorWidth;
        WaitingIndicator.Height = DialogTheme.IndicatorHeight;
        WaitingIndicator.Fill = new SolidColorBrush(DialogTheme.IndicatorInk);
        WaitingIndicator.Data = TriangleDown(DialogTheme.IndicatorWidth, DialogTheme.IndicatorHeight);

        // The countdown is the plate's bottom edge, not a widget in the text column: full
        // width, no track behind it, square (DialogBox's corner radius does the rounding).
        // Nothing else in the box moves when it appears.
        CountdownTrack.Height = DialogTheme.CountdownHeight;
        CountdownFill.Background = _countdownInk;

        FontProbeBundled.FontFamily = face;
        FontProbeBundled.FontSize = DialogTheme.PrimarySize;
        FontProbeSystem.FontSize = DialogTheme.PrimarySize;
    }

    private static void StyleText(TextBlock block, FontFamily face, double size,
        double lineHeight, int maxLines, Windows.UI.Color ink)
    {
        block.FontFamily = face;
        block.FontSize = size;
        if (lineHeight > 0) block.LineHeight = lineHeight;
        block.MaxLines = maxLines;
        block.TextWrapping = maxLines > 1 ? TextWrapping.Wrap : TextWrapping.NoWrap;
        block.TextTrimming = TextTrimming.CharacterEllipsis;
        block.Foreground = new SolidColorBrush(ink);
    }

    /// <summary>Top-to-bottom gradient. Both the plate and its stroke use one: a
    /// flat stroke is exactly the hard white line that makes a box look printed on
    /// rather than lit.</summary>
    private static LinearGradientBrush VerticalGradient(Windows.UI.Color top, Windows.UI.Color bottom)
    {
        var brush = new LinearGradientBrush
        {
            StartPoint = new Point(0, 0),
            EndPoint = new Point(0, 1),
        };
        brush.GradientStops.Add(new GradientStop { Color = top, Offset = 0 });
        brush.GradientStops.Add(new GradientStop { Color = bottom, Offset = 1 });
        return brush;
    }

    /// <summary>The waiting triangle as our own geometry, so it is crisp and shaped
    /// the way we want instead of however a font happens to draw U+25BC.</summary>
    private static Geometry TriangleDown(double width, double height)
    {
        var figure = new PathFigure { StartPoint = new Point(0, 0), IsClosed = true, IsFilled = true };
        figure.Segments.Add(new PolyLineSegment
        {
            Points = { new Point(width, 0), new Point(width / 2, height) },
        });
        var geometry = new PathGeometry();
        geometry.Figures.Add(figure);
        return geometry;
    }

    // -- content -------------------------------------------------------------

    /// <summary>Which reveal preset is configured. The presenter drives the typewriter; the
    /// other two are animations this window owns.</summary>
    internal RevealStyle Reveal => _options.Style;

    /// <summary>
    /// Start a new utterance. The layout is built from the FULL text either way, so
    /// annotations never re-flow mid-reveal; what differs is what is on screen at frame one:
    /// the typewriter starts empty and grows, the other presets start complete and animate
    /// opacity, which is why they have zero layout movement.
    /// </summary>
    public void BeginUtterance(string primary, string? secondary,
        IReadOnlyList<(string Base, string Ruby)>? pairs)
    {
        SetHistoryBadge(null);
        BuildLines(primary, secondary, pairs);

        if (Reveal == RevealStyle.Typewriter)
        {
            Fill(0, 0);
        }
        else
        {
            Fill(int.MaxValue, int.MaxValue);
            foreach (var segment in _segments) segment.Element.Opacity = 0;
        }

        // Lay out now so region and placement match the box the moment it appears, and snap
        // rather than animate: growing up from the previous utterance's height would animate
        // a size the new line never had.
        RootGrid.UpdateLayout();
        SyncBoxBounds(snap: true);
        ShowNow();
    }

    /// <summary>
    /// The typewriter's per-tick call: reveal this much of the base line and this much of
    /// its annotation over the layout <see cref="BeginUtterance"/> already fixed. The box
    /// is NOT snapped here - it follows the new height, which is the plate opening as the
    /// line arrives.
    /// </summary>
    public void RevealText(string prefix, string sourcePrefix)
    {
        Fill(prefix.Length, sourcePrefix.Length);
        SyncBoxBounds();
    }

    /// <summary>
    /// Run the configured non-typewriter reveal over the already-placed layout.
    ///
    /// `fade`: each segment fades up in turn, a clause-sized pause apart - print-like, no
    /// wipe edge. `sweep`: the same fades, but each segment's start time is proportional to
    /// its x position, so a soft band of light travels the block at one speed regardless of
    /// how many segments the line has. Granularity is the segment, not the pixel: a true
    /// per-pixel feather needs a Composition mask over a visual surface, which is a lot of
    /// machinery for a difference nobody can see at 11 dip.
    /// </summary>
    public void RevealComplete(Action onFinished)
    {
        if (_segments.Count == 0)
        {
            onFinished();
            return;
        }

        // X is a PER-LINE offset (BuildLines resets it on every wrapped row), so the last
        // segment is the last line's tail, which is usually the shortest. Normalising by it
        // would put every earlier, wider line at a multiple of SweepTravel - a two-line
        // utterance would sweep for seconds and hold the dwell countdown off for as long.
        double blockWidth = 1;
        foreach (var (_, x, width) in _segments) blockWidth = Math.Max(blockWidth, x + width);
        var duration = Reveal == RevealStyle.Sweep
            ? DialogTheme.SweepFeather
            : DialogTheme.FadeInSegment;

        _reveal?.Stop();
        _reveal = new Storyboard();

        for (int i = 0; i < _segments.Count; i++)
        {
            var (element, x, _) = _segments[i];
            var start = Reveal == RevealStyle.Sweep
                ? DialogTheme.SweepTravel * (x / blockWidth)
                : DialogTheme.FadeStagger * i;

            var animation = new DoubleAnimation
            {
                From = 0,
                To = 1,
                BeginTime = start,
                Duration = new Duration(duration),
                EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            };
            Storyboard.SetTarget(animation, element);
            Storyboard.SetTargetProperty(animation, "Opacity");
            _reveal.Children.Add(animation);
        }

        _reveal.Completed += (_, _) => onFinished();
        _reveal.Begin();
    }

    /// <summary>Put a whole line up at once - a backtracked utterance, or a recall of
    /// the current one. No typewriter and no growth: this is reading, not speaking. The
    /// caller owns the band readout, because only it knows where in the backlog this
    /// came from.</summary>
    public void ShowLine(string primary, string? secondary,
        IReadOnlyList<(string Base, string Ruby)>? pairs)
    {
        BuildLines(primary, secondary, pairs);
        Fill(int.MaxValue, int.MaxValue);
        RootGrid.UpdateLayout();
        SyncBoxBounds(snap: true);
        ShowNow();
    }

    /// <summary>Band readout while backtracking; null clears it.</summary>
    public void SetHistoryBadge(string? text)
    {
        HistoryBadge.Text = text ?? string.Empty;
        HistoryBadge.Visibility = text is null ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>
    /// Rebuild <see cref="LineStack"/> for one utterance. Two shapes, chosen by
    /// <see cref="RubyLayout"/> from the text itself:
    ///   * paired - a row per wrapped line, each row a sequence of clause cells, each cell's
    ///     annotation centred on the clause it belongs to;
    ///   * gloss - the same rows without annotations, plus ONE run carrying the whole spoken
    ///     line above or below the block, because the two sides could not be paired and
    ///     pretending otherwise would put the wrong words under each other.
    /// Rebuilt per utterance, never per reveal - the reveal only fills it.
    /// </summary>
    private void BuildLines(string primary, string? secondary,
        IReadOnlyList<(string Base, string Ruby)>? pairs)
    {
        _layout = RubyLayout.Build(primary, secondary, pairs, AvailableTextWidth(),
            WrapProbe, RubyProbe, DialogTheme.PrimaryMaxLines);

        // Everything below is rebuilt from scratch; leaving the old rows in place is how the
        // box grows by a line per utterance.
        LineStack.Children.Clear();
        _cells.Clear();
        _segments.Clear();
        _gloss = null;
        _reveal?.Stop();
        _reveal = null;

        var rows = new List<UIElement>();
        foreach (var line in _layout.Lines)
        {
            // One horizontal row of segments: the utterance reads straight across it.
            var row = new StackPanel { Orientation = Orientation.Horizontal };
            double lineWidth = line.Sum(c => c.Width);
            double consumed = 0;

            foreach (var cell in line)
            {
                var baseText = new TextBlock();
                StyleText(baseText, _face, DialogTheme.PrimarySize, DialogTheme.PrimaryLineHeight,
                    1, DialogTheme.PrimaryInk);
                baseText.TextWrapping = TextWrapping.NoWrap;
                baseText.TextTrimming = TextTrimming.None;

                var stack = new StackPanel { Spacing = DialogTheme.AnnotationGap };
                stack.Children.Add(baseText);

                TextBlock? ruby = null;
                if (cell.Ruby.Length > 0)
                {
                    ruby = NewRuby();
                    ruby.FontSize = cell.RubySize;

                    // The annotation sits on a Canvas so it CANNOT affect layout width: it is
                    // centred on its segment and allowed to overhang, which is what ruby
                    // does. Letting it widen the cell instead spreads the segments apart and
                    // the utterance stops reading as one line.
                    var layer = new Canvas
                    {
                        Width = cell.Width,
                        Height = DialogTheme.SecondaryLineHeight,
                        IsHitTestVisible = false,
                    };

                    // Centre, then keep the overhang INSIDE the line: it may hang over its
                    // neighbours, never past the first or last segment. Without this the
                    // first segment's annotation runs off the text column and onto the
                    // portrait.
                    double offset = (cell.Width - cell.RubyWidth) / 2;
                    double leftRoom = consumed;
                    double rightRoom = lineWidth - consumed - cell.Width;
                    offset = Math.Max(offset, -leftRoom);
                    offset = Math.Min(offset, cell.Width - cell.RubyWidth + rightRoom);
                    Canvas.SetLeft(ruby, offset);
                    layer.Children.Add(ruby);

                    if (_options.AnnotationAbove) stack.Children.Insert(0, layer);
                    else stack.Children.Add(layer);
                }

                row.Children.Add(stack);
                _cells.Add((baseText, ruby, cell));
                _segments.Add((stack, consumed, cell.Width));
                consumed += cell.Width;
            }

            rows.Add(row);
        }

        if (_layout.Gloss.Length > 0)
        {
            // One run for the whole utterance: no positional claim, so it wraps freely and
            // takes the block's width.
            _gloss = NewRuby();
            _gloss.TextWrapping = TextWrapping.Wrap;
            _gloss.MaxLines = DialogTheme.SecondaryMaxLines;
            _gloss.TextTrimming = TextTrimming.CharacterEllipsis;
            _gloss.Margin = _options.AnnotationAbove
                ? new Thickness(0, 0, 0, DialogTheme.AnnotationGap)
                : new Thickness(0, DialogTheme.AnnotationGap, 0, 0);

            _segments.Add((_gloss, 0, _layout.Lines.Count > 0 ? _layout.Lines[0].Sum(c => c.Width) : 0));
            if (_options.AnnotationAbove) rows.Insert(0, _gloss);
            else rows.Add(_gloss);
        }

        foreach (var row in rows) LineStack.Children.Add(row);
    }

    /// <summary>An annotation run: small, dim, and never taller than it has to be.</summary>
    private TextBlock NewRuby()
    {
        var ruby = new TextBlock { Visibility = Visibility.Collapsed };
        StyleText(ruby, _face, DialogTheme.SecondarySize, DialogTheme.SecondaryLineHeight,
            1, DialogTheme.SecondaryInk);
        ruby.TextWrapping = TextWrapping.NoWrap;
        ruby.TextTrimming = TextTrimming.None;
        return ruby;
    }

    /// <summary>
    /// Spread a revealed prefix over the layout: fill each clause before starting the next,
    /// and give the annotation - per clause when paired, the single gloss otherwise - the
    /// same treatment. Both prefixes advance at one fraction (the typewriter's), so the
    /// utterance and what was spoken always arrive together in time.
    /// </summary>
    private void Fill(int primaryChars, int rubyChars)
    {
        int baseLeft = primaryChars;
        int rubyLeft = rubyChars;

        foreach (var (baseText, ruby, cell) in _cells)
        {
            int baseTake = Math.Clamp(baseLeft, 0, cell.Base.Length);
            baseText.Text = cell.Base[..baseTake];
            baseLeft -= baseTake;

            if (ruby is null) continue;
            int rubyTake = Math.Clamp(rubyLeft, 0, cell.Ruby.Length);
            ruby.Text = cell.Ruby[..rubyTake];
            ruby.Visibility = rubyTake > 0 ? Visibility.Visible : Visibility.Collapsed;
            rubyLeft -= rubyTake;
        }

        if (_gloss is not null)
        {
            int take = Math.Clamp(rubyLeft, 0, _layout.Gloss.Length);
            _gloss.Text = _layout.Gloss[..take];
            _gloss.Visibility = take > 0 ? Visibility.Visible : Visibility.Collapsed;
        }
    }

    /// <summary>Width the text column may use: the box's max, less its own chrome and the
    /// character column when one is showing. Wrapping has to be computed against this, not
    /// against the current (still growing) box.</summary>
    private double AvailableTextWidth()
    {
        double width = DialogBox.MaxWidth
            - DialogTheme.BoxBorderThickness * 2
            - DialogTheme.BodyPadding.Left - DialogTheme.BodyPadding.Right;

        if (CharacterColumn.Visibility == Visibility.Visible)
            width -= DialogTheme.AvatarSize + DialogTheme.ColumnSpacing;

        return Math.Max(40, width);
    }

    /// <summary>Character seam: portrait plus the name under it, or nothing.</summary>
    public void SetCharacter(string name, string? avatarPath, string? expression)
    {
        bool hasName = !string.IsNullOrWhiteSpace(name);
        NameText.Text = hasName
            ? (string.IsNullOrWhiteSpace(expression) ? name : $"{name}·{expression}")
            : string.Empty;
        NameText.Visibility = hasName ? Visibility.Visible : Visibility.Collapsed;

        // A pack-relative avatar is resolved against the data dir, which is only absolute
        // when VC_DATA_DIR is: `new Uri` throws on a relative path, and this runs inside the
        // SSE event handler with nothing above it to catch. A missing portrait is a missing
        // portrait, never a dead tray.
        Uri? avatar = null;
        if (!string.IsNullOrWhiteSpace(avatarPath) && File.Exists(avatarPath))
        {
            try { Uri.TryCreate(Path.GetFullPath(avatarPath), UriKind.Absolute, out avatar); }
            catch { avatar = null; }
        }
        bool hasAvatar = avatar is not null;
        AvatarImage.Source = hasAvatar ? new BitmapImage(avatar!) : null;
        AvatarFrame.Visibility = hasAvatar ? Visibility.Visible : Visibility.Collapsed;

        // The column carries the portrait AND the name, so it goes when both do.
        CharacterColumn.Visibility = hasName || hasAvatar ? Visibility.Visible : Visibility.Collapsed;
    }

    // -- visibility and motion -----------------------------------------------

    private void ShowNow()
    {
        _fade?.Stop();
        _fade = null;

        bool wasShown = _shown;
        // Opacity stays 1 and the fade-in animates FROM 0: if the compositor is not
        // ready to run a storyboard yet (the very first show, before the window has
        // ever rendered), the dialog appears instantly instead of staying invisible at
        // opacity 0 forever.
        RootGrid.Opacity = 1;
        AppWindow.Show(activateWindow: false);
        _shown = true;
        AssertAlwaysOnTop();

        if (!wasShown) Animate(RootGrid, 0, 1, DialogTheme.FadeIn, null);
    }

    /// <summary>
    /// Put the window back in the topmost band if it is not there.
    ///
    /// <c>OverlappedPresenter.IsAlwaysOnTop = true</c> set in the constructor - before
    /// the window has ever been shown - is remembered by the presenter but never
    /// reaches the HWND: the exstyle comes back without WS_EX_TOPMOST, and the dialog
    /// renders *behind* whatever the user is reading, which looks exactly like "the
    /// overlay never appeared". A raw <c>SetWindowPos(HWND_TOPMOST)</c> is not a way
    /// out either; it returns TRUE and changes nothing, because the presenter owns the
    /// window's z-order. Re-assigning the property is what actually applies it, and
    /// the exstyle test keeps that to the one call where it is needed (no z-order
    /// churn on every relayout).
    /// </summary>
    private void AssertAlwaysOnTop()
    {
        if ((GetWindowLongPtr(_hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST) != 0) return;
        var presenter = (OverlappedPresenter)AppWindow.Presenter;
        presenter.IsAlwaysOnTop = false;
        presenter.IsAlwaysOnTop = true;
    }

    /// <summary>
    /// Smooth dismissal. <paramref name="onHidden"/> runs once the window is hidden;
    /// if a new utterance supersedes the fade it never runs, which is correct -
    /// whoever superseded it already owns the state.
    /// </summary>
    public void FadeOut(Action? onHidden)
    {
        if (!_shown)
        {
            onHidden?.Invoke();
            return;
        }
        Animate(RootGrid, 1, 0, DialogTheme.FadeOut, () =>
        {
            HideNow();
            onHidden?.Invoke();
        });
    }

    /// <summary>Hide without animating (master switch off, or the fade landed).</summary>
    public void HideNow()
    {
        _fade?.Stop();
        _fade = null;
        RootGrid.Opacity = 1;
        SetWaiting(false);
        StopCountdown();
        StopGrow();
        AppWindow.Hide();
        _shown = false;
    }

    /// <summary>
    /// The "waiting for you" indicator: the Hold state made visible.
    ///
    /// It breathes in opacity and bobs a few pixels with an eased curve, rather
    /// than hard blinking. A linear on/off blink on a saturated glyph is what reads
    /// as a cheap web widget; slow easing on a near-white triangle reads as craft,
    /// and costs the same two animations.
    /// </summary>
    public void SetWaiting(bool waiting)
    {
        if (waiting)
        {
            if (_blink is not null) return;
            var ease = new SineEase { EasingMode = EasingMode.EaseInOut };
            var breathe = new DoubleAnimation
            {
                From = 1.0,
                To = DialogTheme.IndicatorDimOpacity,
                Duration = new Duration(DialogTheme.IndicatorBlink),
                AutoReverse = true,
                RepeatBehavior = RepeatBehavior.Forever,
                EasingFunction = ease,
            };
            Storyboard.SetTarget(breathe, WaitingIndicator);
            Storyboard.SetTargetProperty(breathe, "Opacity");

            var bob = new DoubleAnimation
            {
                From = 0,
                To = DialogTheme.IndicatorBob,
                Duration = new Duration(DialogTheme.IndicatorBlink),
                AutoReverse = true,
                RepeatBehavior = RepeatBehavior.Forever,
                EasingFunction = ease,
            };
            Storyboard.SetTarget(bob, IndicatorShift);
            Storyboard.SetTargetProperty(bob, "Y");

            _blink = new Storyboard();
            _blink.Children.Add(breathe);
            _blink.Children.Add(bob);
            _blink.Begin();
        }
        else
        {
            _blink?.Stop();
            _blink = null;
            WaitingIndicator.Opacity = 0;
            IndicatorShift.Y = 0;
        }
    }

    /// <summary>
    /// Run the dwell countdown as ONE compositor animation over the whole dwell, not a
    /// value pushed from a timer: a 100 ms tick moves the bar in ~1% steps and reads as
    /// stutter, while a single storyboard is interpolated per frame off the UI thread.
    /// <paramref name="onElapsed"/> fires when the bar reaches zero.
    /// </summary>
    public void StartCountdown(double seconds, Action onElapsed)
    {
        StopCountdown();

        CountdownScale.ScaleX = 1;
        CountdownFill.Background = _countdownInk;
        CountdownTrack.Visibility = Visibility.Visible;

        var drain = new DoubleAnimation
        {
            From = 1,
            To = 0,
            Duration = new Duration(TimeSpan.FromSeconds(Math.Max(seconds, 0.2))),
            EnableDependentAnimation = true,
        };
        Storyboard.SetTarget(drain, CountdownScale);
        Storyboard.SetTargetProperty(drain, "ScaleX");

        _countdown = new Storyboard();
        _countdown.Children.Add(drain);
        _countdown.Completed += (_, _) => onElapsed();
        _countdown.Begin();
    }

    /// <summary>Hover freeze: pause the animation where it is and say so in colour.
    /// Pausing beats re-deriving a deadline - the storyboard already holds the
    /// remaining time, exactly.</summary>
    public void SetCountdownFrozen(bool frozen)
    {
        if (_countdown is null) return;
        if (frozen) _countdown.Pause();
        else _countdown.Resume();

        var ink = frozen ? _countdownFrozenInk : _countdownInk;
        if (!ReferenceEquals(CountdownFill.Background, ink)) CountdownFill.Background = ink;
    }

    public void StopCountdown()
    {
        _countdown?.Stop();
        _countdown = null;
        CountdownTrack.Visibility = Visibility.Collapsed;
        CountdownScale.ScaleX = 1;
    }

    /// <summary>
    /// One opacity storyboard at a time. <paramref name="from"/> is explicit so a
    /// storyboard that never gets to run leaves the element at its declarative value
    /// rather than at an animation's start value.
    /// </summary>
    private void Animate(UIElement target, double from, double to, TimeSpan duration,
        Action? completed)
    {
        var animation = new DoubleAnimation
        {
            From = from,
            To = to,
            Duration = new Duration(duration),
        };
        Storyboard.SetTarget(animation, target);
        Storyboard.SetTargetProperty(animation, "Opacity");

        var storyboard = new Storyboard();
        storyboard.Children.Add(animation);
        if (completed is not null) storyboard.Completed += (_, _) => completed();

        _fade?.Stop();
        _fade = storyboard;
        storyboard.Begin();
    }

    // -- geometry ------------------------------------------------------------

    /// <summary>
    /// Measure the box the text needs, then let the window follow it in BOTH dimensions
    /// (see <see cref="TickGrow"/>): a new line starts as a small plate and opens
    /// sideways and upward as the words arrive, settling when the sentence is complete.
    /// Also refreshes the caption and passthrough regions (CLIENT space) from the
    /// arranged result.
    ///
    /// The size comes from an explicit <c>Measure</c> against the box's MAX width, not
    /// from <c>ActualWidth/ActualHeight</c>: the XAML root is constrained by the client
    /// area this method is about to resize, so reading the arranged size would pin the
    /// box to the size it already has and no utterance could ever grow it.
    /// </summary>
    /// <param name="snap">Apply the measured size immediately instead of animating to
    /// it: a new utterance, a DPI change or a recalled line has no motion to show.</param>
    private void SyncBoxBounds(bool snap = false)
    {
        if (_inSync || DialogBox.MaxWidth <= 0) return;
        // While the growth timer runs it OWNS the measuring (see StartGrow): it re-measures
        // at the top of every frame and stops itself on the frame it settles. Measuring here
        // as well - once per revealed character - runs the same full layout pass two or three
        // times per frame and its result is overwritten by TickGrow before anything reads it.
        if (!snap && _shown && _grow is { IsRunning: true }) return;
        _inSync = true;
        try
        {
            if (!MeasureTarget()) return;

            if (snap || !_shown)
            {
                StopGrow();
                _appliedWidth = _targetWidth;
                _appliedHeight = _targetHeight;
                ApplyBox(_targetWidth, _targetHeight);
                RootGrid.UpdateLayout();
                SyncRegions();
            }
            else if (Math.Abs(_targetWidth - _appliedWidth) >= 1 ||
                     Math.Abs(_targetHeight - _appliedHeight) >= 1)
            {
                // Regions are refreshed once, when the motion settles: the band's rect
                // moves every frame while the box opens, and re-punching the caption hole
                // 60 times a second is UI-thread work nobody can see.
                StartGrow();
            }
            else
            {
                SyncRegions();
            }
        }
        finally
        {
            _inSync = false;
        }
    }

    /// <summary>Measure the size the content wants, into the growth targets. False when
    /// there is nothing to measure yet.</summary>
    private bool MeasureTarget()
    {
        using (_metrics.TimeMeasure())
        {
            DialogBox.Measure(new Size(DialogBox.MaxWidth, double.PositiveInfinity));
        }

        var desired = DialogBox.DesiredSize;
        if (desired.Width <= 0 || desired.Height <= 0) return false;

        double scale = Scale;
        _targetWidth = Math.Ceiling(desired.Width * scale);
        _targetHeight = Math.Ceiling(desired.Height * scale);
        return true;
    }

    /// <summary>
    /// Re-punch the caption and passthrough regions from the arranged layout.
    ///
    /// Only the band is caption: the body must stay client area so XAML sees the click
    /// that dismisses the dialog. The micro-controls live inside the band, so they need a
    /// Passthrough hole - and WM_NCHITTEST has to answer HTCLIENT over the same rect,
    /// because our own handler runs first and a caption answer there hands the press to
    /// the OS move loop, which is exactly what made the icons behave like decoration.
    /// </summary>
    private void SyncRegions()
    {
        // TransformToVisual throws before the first arrange, which is where
        // BeginUtterance calls in from on a window that has never been shown; the regions
        // land on the relayout that follows it.
        if (TopBand.ActualHeight <= 0) return;

        double scale = Scale;
        var controls = RectFor(BandControls, scale);
        if (controls.Width <= 0 || controls.Height <= 0) return;

        // A few pixels of slack: the icon boxes are tight and a hairline miss means the
        // click silently becomes a window drag.
        const int pad = 6;
        var hole = new RectInt32(controls.X - pad, controls.Y - pad,
            controls.Width + pad * 2, controls.Height + pad * 2);
        if (hole.X == _controlsPx.X && hole.Y == _controlsPx.Y &&
            hole.Width == _controlsPx.Width && hole.Height == _controlsPx.Height &&
            _boxPx.Width == _bandPx.Width && _boxPx.Height == _bandPx.Height) return;

        // The WHOLE box is caption, so a left press anywhere drags it, with one hole cut
        // out for the controls. Passthrough is evaluated ahead of Caption.
        _bandPx = new RectInt32(0, 0, _boxPx.Width, _boxPx.Height);
        _controlsPx = hole;
        _nonClient.SetRegionRects(NonClientRegionKind.Caption, new[] { _bandPx });
        _nonClient.SetRegionRects(NonClientRegionKind.Passthrough, new[] { _controlsPx });

        _metrics.RegionUpdates++;
    }

    /// <summary>Apply a box size: this is the only place the window's client rect is
    /// decided, so placement can stay a pure function of it.</summary>
    private void ApplyBox(double width, double height)
    {
        var box = new RectInt32(0, 0, (int)Math.Round(width), (int)Math.Round(height));
        if (box.Width == _boxPx.Width && box.Height == _boxPx.Height) return;
        _boxPx = box;
        using (_metrics.TimeResize())
        {
            ApplyAnchorPosition();
        }
    }

    /// <summary>
    /// Chase the measured size a fraction of the remaining gap per frame, in both
    /// dimensions: the plate opens sideways as words are added and upward when the line
    /// wraps. An exponential follow rather than a fixed-duration animation because the
    /// target moves while the animation runs - every typewriter tick can push it
    /// further, and a duration-based tween would have to be restarted (and would visibly
    /// re-ease) on every one of them.
    ///
    /// This timer, not the reveal, owns the measuring. A reveal fires per character; if
    /// each one measured, forced a layout pass and resized the window, a long line meant
    /// hundreds of synchronous SetWindowPos + DWM recompositions on the UI thread - and
    /// because the tray's low-level mouse hook lives on that thread, that is felt as a
    /// stuttering cursor across the whole desktop. One measure per frame, at most.
    /// </summary>
    private void StartGrow()
    {
        if (_grow is null)
        {
            _grow = DispatcherQueue.CreateTimer();
            _grow.Interval = DialogTheme.GrowTick;
            _grow.Tick += (_, _) => TickGrow();
        }
        if (!_grow.IsRunning)
        {
            _growClock.Restart();
            _grow.Start();
        }
    }

    private void StopGrow() => _grow?.Stop();

    private void TickGrow()
    {
        // How late this frame was IS the UI-thread stall, so it is recorded rather than
        // guessed at from a symptom.
        _metrics.RecordTick(_growClock.Elapsed.TotalMilliseconds, DialogTheme.GrowTick.TotalMilliseconds);
        _growClock.Restart();
        _metrics.GrowFrames++;

        if (_inSync) return;
        _inSync = true;
        try
        {
            // The text may have grown since the last frame; re-measuring here is what
            // keeps the reveal itself free of layout work.
            if (!MeasureTarget()) return;

            double widthGap = _targetWidth - _appliedWidth;
            double heightGap = _targetHeight - _appliedHeight;

            if (Math.Abs(widthGap) < DialogTheme.GrowSnapPx &&
                Math.Abs(heightGap) < DialogTheme.GrowSnapPx)
            {
                _appliedWidth = _targetWidth;
                _appliedHeight = _targetHeight;
                // Exact on the settling frame; the quantum below is for the motion only.
                ApplyBox(_appliedWidth, _appliedHeight);
                RootGrid.UpdateLayout();
                StopGrow();
                SyncRegions();
                return;
            }

            _appliedWidth += widthGap * DialogTheme.GrowFollowPerFrame;
            _appliedHeight += heightGap * DialogTheme.GrowFollowPerFrame;
            ApplyBox(Quantize(_appliedWidth), Quantize(_appliedHeight));
            // Arrange into the size the window just became, in this same frame: otherwise
            // the client area is bigger than the arranged plate until XAML's next layout
            // pass, and that strip shows whatever erased it rather than the plate.
            RootGrid.UpdateLayout();
        }
        finally
        {
            _inSync = false;
        }
    }

    /// <summary>Round an animated size to the resize quantum. Every distinct value costs
    /// a full window resize (~5 ms of UI thread), and single-pixel steps of a plate that
    /// is opening are not something anyone can see.</summary>
    private static double Quantize(double px) =>
        Math.Round(px / DialogTheme.GrowQuantumPx) * DialogTheme.GrowQuantumPx;

    /// <summary>An element's bounds in CLIENT physical pixels.</summary>
    private RectInt32 RectFor(FrameworkElement element, double scale)
    {
        var origin = element.TransformToVisual(RootGrid).TransformPoint(new Point(0, 0));
        int left = (int)Math.Floor(origin.X * scale);
        int top = (int)Math.Floor(origin.Y * scale);
        int right = (int)Math.Ceiling((origin.X + element.ActualWidth) * scale);
        int bottom = (int)Math.Ceiling((origin.Y + element.ActualHeight) * scale);
        return new RectInt32(left, top, right - left, bottom - top);
    }

    /// <summary>Client-area origin relative to the window origin, in physical pixels.
    /// A window with a border has a non-zero one, and every placement calculation is
    /// expressed in terms of where the BOX (the client area) has to land.</summary>
    private (int x, int y) ClientOffset()
    {
        var point = new POINTL();
        if (!ClientToScreen(_hwnd, ref point) || !GetWindowRect(_hwnd, out var window)) return (0, 0);
        return (point.X - window.Left, point.Y - window.Top);
    }

    /// <summary>Window size minus client size, in physical pixels: what
    /// <c>MoveAndResize</c> (window space) needs on top of a client-space size.</summary>
    private (int w, int h) FrameSlack()
    {
        if (!GetWindowRect(_hwnd, out var window) || !GetClientRect(_hwnd, out var client)) return (0, 0);
        return (window.Right - window.Left - (client.Right - client.Left),
                window.Bottom - window.Top - (client.Bottom - client.Top));
    }

    /// <summary>
    /// Frosted plate, drawn by the system. <c>Window.SystemBackdrop</c> is not usable
    /// here: it stops sampling when the window is not input-active, and this window is
    /// WS_EX_NOACTIVATE, so it would show a flat fallback tint forever. Owning the
    /// configuration lets the window claim <c>IsInputActive</c> permanently.
    /// </summary>
    private void EnableAcrylic()
    {
        if (!DesktopAcrylicController.IsSupported()) return;

        _backdropConfig = new SystemBackdropConfiguration
        {
            IsInputActive = true,
            Theme = SystemBackdropTheme.Dark,
        };

        _acrylic = new DesktopAcrylicController
        {
            Kind = DesktopAcrylicKind.Base,
            TintColor = DialogTheme.AcrylicTint,
            TintOpacity = DialogTheme.AcrylicTintOpacity,
            LuminosityOpacity = DialogTheme.AcrylicLuminosityOpacity,
            FallbackColor = DialogTheme.AcrylicFallback,
        };
        _acrylic.AddSystemBackdropTarget(
            WinRT.CastExtensions.As<ICompositionSupportsSystemBackdrop>(this));
        _acrylic.SetSystemBackdropConfiguration(_backdropConfig);
    }

    private double Scale
    {
        get
        {
            uint dpi = GetDpiForWindow(_hwnd);
            return dpi == 0 ? 1.0 : dpi / 96.0;
        }
    }

    private RectInt32 WorkArea =>
        DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest).WorkArea;

    /// <summary>Set the width band the box may occupy. Inside it the box hugs its text,
    /// which is what lets a new line open sideways; <see cref="SyncBoxBounds"/> turns the
    /// measured result into the window's client size.</summary>
    private void ApplyBoxWidth()
    {
        // On a display too narrow for the design width, the plate shrinks to fit rather
        // than hanging off the edge.
        double availableDip = WorkArea.Width / Scale - 2 * DialogTheme.BottomGapDip;
        DialogBox.MinWidth = DialogTheme.BoxMinWidth;
        DialogBox.MaxWidth = Math.Max(DialogTheme.BoxMinWidth,
            Math.Min(DialogTheme.BoxMaxWidth, availableDip));
    }

    /// <summary>
    /// Size the window to the box and move it so the box's bottom-center lands on the
    /// anchor, clamped to keep the whole box inside the work area. Anchoring the bottom
    /// edge is what makes long lines grow upward instead of pushing off screen, and it
    /// keeps a stale saved anchor (e.g. written by an older build) from parking the box
    /// out of sight. Size and position go in one call: two calls would show the box at
    /// the old position in its new size for a frame.
    /// </summary>
    private void ApplyAnchorPosition()
    {
        if (_boxPx.Width <= 0 || _boxPx.Height <= 0) return;

        var area = WorkArea;
        var anchor = _anchor ?? DefaultAnchor(area);
        int left = Math.Clamp(anchor.X - _boxPx.Width / 2, area.X,
            Math.Max(area.X, area.X + area.Width - _boxPx.Width));
        int top = Math.Clamp(anchor.Y - _boxPx.Height, area.Y,
            Math.Max(area.Y, area.Y + area.Height - _boxPx.Height));

        var (offsetX, offsetY) = ClientOffset();
        var (slackW, slackH) = FrameSlack();
        AppWindow.MoveAndResize(new RectInt32(
            left - offsetX, top - offsetY,
            _boxPx.Width + slackW, _boxPx.Height + slackH));

        // A relayout re-moves the window (the box grows with the text), and a move can
        // take it out of the topmost band; an overlay that is not topmost is not an
        // overlay, so the band is re-asserted here too.
        AssertAlwaysOnTop();
    }

    private PointInt32 DefaultAnchor(RectInt32 area) => new(
        area.X + area.Width / 2,
        area.Y + area.Height - (int)Math.Round(DialogTheme.BottomGapDip * Scale));

    /// <summary>Box bottom-center in screen pixels, for the current window position.</summary>
    private PointInt32 CurrentAnchor()
    {
        var pos = AppWindow.Position;
        var (offsetX, offsetY) = ClientOffset();
        return new PointInt32(
            pos.X + offsetX + _boxPx.Width / 2,
            pos.Y + offsetY + _boxPx.Height);
    }

    /// <summary>After a drag: clamp back on screen, then remember where the box sits.</summary>
    private void PersistAnchor()
    {
        _anchor = CurrentAnchor();
        ApplyAnchorPosition();
        _anchor = CurrentAnchor();
        SubtitleOptions.SaveAnchor(_dataDir, _anchor.Value.X, _anchor.Value.Y);
    }

    /// <summary>Forget the dragged position and snap back to bottom-center.</summary>
    public void ResetPosition()
    {
        _anchor = null;
        SubtitleOptions.ClearAnchor(_dataDir);
        ApplyAnchorPosition();
    }

    /// <summary>Is this screen point inside the box? Used by the hover freeze and by
    /// the tray's wheel gesture, both of which live outside this window.</summary>
    public bool HitTestScreen(int screenX, int screenY)
    {
        if (!_shown || _boxPx.Width <= 0) return false;
        var pos = AppWindow.Position;
        var (offsetX, offsetY) = ClientOffset();
        int x = screenX - pos.X - offsetX;
        int y = screenY - pos.Y - offsetY;
        return x >= 0 && x < _boxPx.Width && y >= 0 && y < _boxPx.Height;
    }

    /// <summary>Pointer-over test without pointer events: the box is a caption region
    /// in part and never focused, so polling the cursor while a countdown runs is
    /// both cheaper and more reliable than tracking enter/leave messages.</summary>
    public bool IsCursorOverBox =>
        GetCursorPos(out var point) && HitTestScreen(point.X, point.Y);

    // -- native window messages ----------------------------------------------

    private void OnWindowMessage(object? sender, WindowMessageEventArgs e)
    {
        switch (e.Message.MessageId)
        {
            case WM_NCHITTEST:
            {
                // lParam is screen space, the band rect is client space, and a bordered
                // window's client area is inset from its window rect.
                int screenX = unchecked((short)(long)e.Message.LParam);
                int screenY = unchecked((short)((long)e.Message.LParam >> 16));
                var pos = AppWindow.Position;
                var (offsetX, offsetY) = ClientOffset();
                int x = screenX - pos.X - offsetX;
                int y = screenY - pos.Y - offsetY;

                bool onControls =
                    x >= _controlsPx.X && x < _controlsPx.X + _controlsPx.Width &&
                    y >= _controlsPx.Y && y < _controlsPx.Y + _controlsPx.Height;

                // HTCAPTION everywhere -> a left press anywhere on the box starts the OS
                // move loop, which is what makes dragging pixel-exact and lag-free, and
                // gives the right button a WM_NCRBUTTONUP to dismiss on. The control
                // cluster is the one exception: it answers HTCLIENT so the island sees the
                // press, because a caption answer there hands it to the move loop and the
                // icons never get clicked at all.
                e.Result = onControls ? HTCLIENT : HTCAPTION;
                e.Handled = true;
                break;
            }

            case WM_NCLBUTTONDBLCLK:
                // Double-click anywhere: back to bottom-center. Also suppresses the default
                // caption double-click, which is maximize.
                ResetPosition();
                e.Result = 0;
                e.Handled = true;
                break;

            // The box is non-client area, so the right button would raise the system menu.
            // Suppress that and make the release dismiss the dialog.
            case WM_NCRBUTTONDOWN:
                e.Result = 0;
                e.Handled = true;
                break;

            case WM_NCRBUTTONUP:
                CloseRequested?.Invoke();
                e.Result = 0;
                e.Handled = true;
                break;

            case WM_EXITSIZEMOVE:
                PersistAnchor();
                break;

            case WM_DPICHANGED:
                // Let WinUI apply the new scale first, then re-fit width, size, placement.
                DispatcherQueue.TryEnqueue(() =>
                {
                    ApplyBoxWidth();
                    SyncBoxBounds(snap: true);
                });
                break;

            // Every plate that opens is a stream of MoveAndResize calls, and the OS erases
            // each newly exposed strip with the window class brush (white) before XAML gets
            // a layout pass into it - a white edge along the growing side, for one frame per
            // resize. Erasing it with the plate's own fallback colour instead makes that
            // frame indistinguishable from the plate. The acrylic backdrop paints over this
            // on the frame after, so it is only ever seen mid-resize.
            case WM_ERASEBKGND:
                if (GetClientRect(_hwnd, out var erase))
                {
                    if (_eraseBrush == 0) _eraseBrush = CreateSolidBrush(Colorref(DialogTheme.AcrylicFallback));
                    FillRect((nint)e.Message.WParam, ref erase, _eraseBrush);
                }
                e.Result = 1;
                e.Handled = true;
                break;
        }
    }

    /// <summary>A <c>Color</c> as GDI's COLORREF: 0x00BBGGRR, alpha dropped (GDI has
    /// none).</summary>
    private static uint Colorref(Windows.UI.Color color) =>
        (uint)(color.R | (color.G << 8) | (color.B << 16));

    // -- self test (--subtitle-selftest) -------------------------------------

    /// <summary>
    /// Run the layout matrix and dump machine-checkable metrics to
    /// <paramref name="reportPath"/>. This is the text-only iteration loop for the
    /// dialog: no screenshots needed to tell whether a line fits, wraps, trims or
    /// lands on screen. <c>fits_client</c> and <c>onscreen</c> must read yes for
    /// every case; the other numbers move with the visual design.
    /// </summary>
    public async Task RunSelfTestAsync(string reportPath)
    {
        (string Name, string Primary, string? Secondary, (string Base, string Ruby)[]? Pairs)[] cases =
        {
            ("short-cjk", "收到。", null, null),
            ("one-line", "已经把三号机的输出功率调到百分之七十了。", null, null),
            ("two-line", "这段话稍微长一些，用来验证对话框在达到最大宽度之后能够正确换行，而不是把窗口撑破或者把尾巴截掉。", null, null),
            // Equal clause counts: the annotation can be paired clause for clause.
            ("bilingual", "明白了，现在开始处理。", "了解しました、これから処理を始めます。", null),
            // The production path: the caller aligns the two languages itself. The 「，」 pair
            // carries punctuation as its annotation, which must annotate NOTHING rather than
            // hang a lone 「、」 under a comma - expect the same line with one fewer ruby run.
            ("supplied-pairs", "欢迎回来，老师。", "おかえりなさい、先生。", new[]
            {
                ("欢迎回来", "おかえりなさい"),
                ("，", "、"),
                ("老师。", "先生。"),
            }),
            // Unequal clause counts: no positional claim is possible, so it must fall back
            // to a single gloss rather than invent a mapping.
            ("bilingual-gloss", "这是一句较长的话，用来确认对话框会换行，最后稳定下来。",
                "これは長い文で、対話ボックスが横にも縦にも開くことを確認します。", null),
            ("latin", "Rebuilt the overlay: fixed transparent canvas, native caption drag, window region clipped to the dialog box.", null, null),
            ("long-4x", string.Concat(Enumerable.Repeat("这是一段很长的字幕文本，用来确认多行换行与最大行数限制都按预期工作。", 4)), null, null),
            ("overflow-12x", string.Concat(Enumerable.Repeat("超长文本溢出测试：应当在第八行以省略号收尾，而不是把对话框顶出画布。", 12)), null, null),
        };

        var area = WorkArea;
        var report = new StringBuilder();
        report.AppendLine($"# voice-core dialog selftest  {DateTime.Now:yyyy-MM-dd HH:mm:ss}");
        report.AppendLine(string.Create(CultureInfo.InvariantCulture,
            $"scale\t{Scale:0.###}\tbox_max_dip\t{DialogTheme.BoxMaxWidth}\tclient_px\t{AppWindow.ClientSize.Width}x{AppWindow.ClientSize.Height}\twindow_px\t{AppWindow.Size.Width}x{AppWindow.Size.Height}\twork_area\t{area.X},{area.Y},{area.Width}x{area.Height}"));
        report.AppendLine(await MeasureFontAsync());
        report.AppendLine("case\tchars\tbox_dip\tbox_px\tbox_screen\tband_px\tprimary_dip\tlines\ttrimmed\tannotation\trubies\tfits_client\tonscreen");

        foreach (var (name, primary, secondary, pairs) in cases)
        {
            // The finished layout is what matters here, not a frame of the typewriter,
            // so the full line goes in at once and the box snaps to it.
            StopCountdown();
            BuildLines(primary, secondary, pairs);
            Fill(int.MaxValue, int.MaxValue);
            RootGrid.UpdateLayout();
            SyncBoxBounds(snap: true);
            ShowNow();
            await SettleAsync();

            int lines = _layout.Lines.Count;
            bool trimmed = _cells.Count > 0 && _cells[^1].Cell.Base.EndsWith('…');
            // Which correspondence the layout was able to claim: paired clause-for-clause,
            // a whole-line gloss, or nothing to annotate.
            string annotation = _layout.Paired ? "clause" : _layout.Gloss.Length > 0 ? "gloss" : "none";
            // How many cells actually carry a ruby run: a punctuation-only annotation must
            // leave its cell bare, so this is below the cell count whenever that happened.
            int rubies = _cells.Count(c => c.Ruby is not null);
            // The window is the box, so the check is that the client area the OS gave
            // us really is the size XAML laid out - a mismatch means clipped text.
            bool fitsClient =
                AppWindow.ClientSize.Width >= _boxPx.Width &&
                AppWindow.ClientSize.Height >= _boxPx.Height;

            var pos = AppWindow.Position;
            var (offsetX, offsetY) = ClientOffset();
            int screenLeft = pos.X + offsetX;
            int screenTop = pos.Y + offsetY;
            bool onscreen =
                screenLeft >= area.X && screenTop >= area.Y &&
                screenLeft + _boxPx.Width <= area.X + area.Width &&
                screenTop + _boxPx.Height <= area.Y + area.Height;

            report.AppendLine(string.Create(CultureInfo.InvariantCulture,
                $"{name}\t{primary.Length}\t{DialogBox.ActualWidth:0.#}x{DialogBox.ActualHeight:0.#}\t" +
                $"{_boxPx.Width}x{_boxPx.Height}\t{screenLeft},{screenTop}\t" +
                $"{_bandPx.Width}x{_bandPx.Height}\t" +
                $"{LineStack.ActualWidth:0.#}x{LineStack.ActualHeight:0.#}\t{lines}\t" +
                $"{(trimmed ? "yes" : "no")}\t" +
                $"{annotation}\t{rubies}/{_cells.Count}\t" +
                $"{(fitsClient ? "yes" : "NO")}\t{(onscreen ? "yes" : "NO")}"));
        }

        // What the matrix cost to render, so a regression in the measure/resize loop is
        // visible in the same report as the layout it produced.
        report.AppendLine(_metrics.Summary());
        File.WriteAllText(reportPath, report.ToString());
    }

    /// <summary>Force layout and let one compositor frame land so ActualSize is final.</summary>
    private async Task SettleAsync()
    {
        RootGrid.UpdateLayout();
        await Task.Delay(120);
        RootGrid.UpdateLayout();
    }

    /// <summary>
    /// Measure one probe string in the bundled face and in a system face. Equal
    /// widths mean the bundled font never loaded and WinUI silently fell back - the
    /// one failure mode a FontFamily string cannot reveal.
    /// </summary>
    private async Task<string> MeasureFontAsync()
    {
        const string probe = "字幕 Subtitle 0123 かな";
        FontProbeBundled.Text = probe;
        FontProbeSystem.Text = probe;
        await SettleAsync();

        return string.Create(CultureInfo.InvariantCulture,
            $"font\t{WrapProbe.FontFamily.Source}\tprobe_bundled_dip\t{FontProbeBundled.ActualWidth:0.#}" +
            $"\tprobe_system_dip\t{FontProbeSystem.ActualWidth:0.#}");
    }
}
