using H.NotifyIcon;
using Microsoft.UI;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using System.Diagnostics;
using VoiceCoreTray.Dialog;
using VoiceCoreTray.Services;
using Windows.Graphics;

namespace VoiceCoreTray;

/// <summary>
/// Tray host and status surface. This window is a presenter: it renders what the
/// runtime reports and forwards utterances to the <see cref="IDialogPresenter"/>. It
/// owns no processes, no ports and no model paths — v1's equivalent owned all three,
/// which made the GUI mandatory.
///
/// It does own global input. Hotkeys and the wheel gesture are registered on THIS
/// window: it has a message pump, it outlives every dialog, and the dialog itself is
/// deliberately never focusable, so it could not receive either.
/// </summary>
public sealed partial class MainWindow : Window
{
    private readonly RuntimeClient _runtime = RuntimeClient.Default;
    private readonly DispatcherQueue _dispatcher;
    private readonly SubtitleOptions _options;
    private readonly DialogPresenter _presenter;
    private readonly HotkeyManager _hotkeys;
    private readonly WheelGestureWatcher _wheel;

    private bool _autoplayEnabled = true;
    private string _note = string.Empty;
    private string? _warning;

    public MainWindow(SubtitleOptions options)
    {
        InitializeComponent();
        _dispatcher = DispatcherQueue.GetForCurrentThread();
        _options = options;

        Title = "voice-core";
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "assets", "icon.ico"));
        if (Content is FrameworkElement root)
        {
            root.RequestedTheme = ElementTheme.Default;
        }
        // Mica for the tray/status window; the dialog is a separate transparent window.
        SystemBackdrop = new MicaBackdrop { Kind = MicaKind.BaseAlt };

        var presenter = (OverlappedPresenter)AppWindow.Presenter;
        presenter.IsAlwaysOnTop = true;
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        presenter.SetBorderAndTitleBar(hasBorder: false, hasTitleBar: false);
        AppWindow.IsShownInSwitchers = false;

        RepositionWindow(460, 280);

        TrayIcon.IconSource = LoadIcon();
        TrayIcon.ToolTipText = "voice-core: 连接中...";

        _presenter = new DialogPresenter(_runtime, _options);
        MenuPin.IsChecked = _options.PinMode;

        // Pinning can also be toggled from the dialog's own control, so mirror it
        // back or the menu check would lie.
        _presenter.PinnedChanged += pinned =>
            _dispatcher.TryEnqueue(() => MenuPin.IsChecked = pinned);

        // Reserved control seam: no runtime route exists for speed, volume or branch
        // choice, so an intent is surfaced and goes no further.
        _presenter.ControlRequested += (_, e) =>
            _dispatcher.TryEnqueue(() => Note($"控制事件 {e.Control}={e.Value:0.##}（暂无后端路由）"));

        // One settings file for the whole app; a migration from the old per-feature files
        // happens on first read and reports itself through the status note.
        var config = AppConfig.Load(_runtime.DataDir, Warn);

        _hotkeys = new HotkeyManager(
            WinRT.Interop.WindowNative.GetWindowHandle(this),
            _runtime.DataDir,
            config.Hotkeys,
            Warn,
            new Dictionary<HotkeyAction, Action>
            {
                [HotkeyAction.ToggleDialog] = _presenter.ToggleVisibility,
                [HotkeyAction.ToggleHold] = _presenter.ToggleHold,
            });

        // Wheel over the dialog walks its backlog, Galgame style.
        _wheel = new WheelGestureWatcher(_presenter.ScrollGesture, Note);

        // The runtime pushes nothing at us: we subscribe to its event stream and
        // fetch the audio bytes for each utterance ourselves. The speaker's name and
        // portrait come from the voice pack, resolved once per pack and cached, and ride
        // ALONG with the utterance - a global "current character" would follow the last
        // line spoken instead of the line on screen.
        _runtime.SpeechReceived += async speech =>
        {
            var character = await _runtime.CharacterAsync(speech.VoicePackId);
            _dispatcher.TryEnqueue(() =>
            {
                _presenter.Show(new DialogUtterance(
                    speech.AudioId, speech.DisplayText, speech.Text, speech.Wav,
                    speech.DisplaySeconds, _autoplayEnabled,
                    character?.Name, character?.AvatarPath,
                    speech.RubyPairs?.Select(p => (p.Base, p.Ruby)).ToList()));
            });
        };
        _runtime.StatusNote += note => _dispatcher.TryEnqueue(() => Note(note));
        _runtime.StartSubscription();

        _ = RefreshTrayAsync();

        var healthTimer = DispatcherQueue.CreateTimer();
        healthTimer.Interval = TimeSpan.FromSeconds(5);
        healthTimer.Tick += async (_, _) => await RefreshTrayAsync();
        healthTimer.Start();

        if (!_options.NoRuntime)
        {
            // Launching must not block the UI thread.
            _ = Task.Run(async () =>
            {
                var status = await _runtime.EnsureRunningAsync();
                if (status.Detail is string detail)
                {
                    _dispatcher.TryEnqueue(() => Note(detail));
                }
            });
        }
    }

    private void RepositionWindow(int width, int height)
    {
        var area = DisplayArea.Primary.WorkArea;
        AppWindow.Resize(new SizeInt32(width, height));
        AppWindow.Move(new PointInt32(
            area.X + (area.Width - width) / 2,
            area.Y + area.Height - height - 32));
    }

    private BitmapImage LoadIcon()
    {
        var path = Path.Combine(AppContext.BaseDirectory, "assets", "icon.ico");
        return new BitmapImage(new Uri(path));
    }

    /// <summary>The one place a human-readable line lands: status view plus tooltip.</summary>
    private void Note(string note)
    {
        _note = note;
        StatusNote.Text = note;
    }

    /// <summary>
    /// A note that must not be missed. Hotkey registration failures come through
    /// here: they are silent by nature (the key simply belongs to another app), so
    /// the warning is sticky in the tray tooltip until the tray restarts.
    /// </summary>
    private void Warn(string note)
    {
        _warning = note;
        Note(note);
        _ = RefreshTrayAsync();
    }

    private async Task RefreshTrayAsync()
    {
        var status = await _runtime.StatusAsync();
        var tip = "voice-core: " + status.Summary;
        if (_warning is not null) tip += " ⚠ " + _warning;
        // NOTIFYICONDATA.szTip is 128 wide chars; a long warning must not truncate
        // the part that says which service is running.
        TrayIcon.ToolTipText = tip.Length > 120 ? tip[..120] : tip;
        if (StatusView.Visibility == Visibility.Visible)
        {
            RenderStatus(status);
        }
    }

    // -- status --------------------------------------------------------------

    private void RenderStatus(RuntimeClient.RuntimeStatus status)
    {
        StatusList.Children.Clear();
        foreach (var (glyph, name, state) in new (string, string, string)[]
        {
            // An answer of any code proves a runtime owns 8760, so this row is known even
            // when the request was refused; the two below stay "—" because they are not.
            ("\uE7f4", "runtime 服务",
                status.Reachable || status.HttpStatus is not null ? "运行中" : "已停止"),
            ("\uE720", "声线引擎（Irodori）", status.Reachable
                ? (status.EngineRunning
                    ? (status.ModelLoaded ? "已加载" : "已启动，模型未加载")
                    : "空闲（显存已释放）")
                : "—"),
            ("\uE8d6", "声线包", status.Reachable ? $"{status.VoicePacks} 个" : "—"),
        })
        {
            var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10 };
            row.Children.Add(new FontIcon
            {
                FontFamily = new FontFamily("Segoe Fluent Icons"),
                Glyph = glyph,
                FontSize = 14,
            });
            row.Children.Add(new TextBlock { Text = name, Width = 180 });
            row.Children.Add(new TextBlock { Text = state, Opacity = 0.8 });
            StatusList.Children.Add(row);
        }
        StatusNote.Text = status.Detail ?? _note;
    }

    /// <summary>
    /// 服务状态 toggles, because nothing else can dismiss this window: line 60 removes the
    /// title bar, so the frame has no close affordance, and it is always-on-top and hidden
    /// from Alt+Tab. Hide, never Close — this HWND owns the <c>RegisterHotKey</c>
    /// registrations (<see cref="HotkeyManager"/>) and the thread that pumps the
    /// <c>WH_MOUSE_LL</c> callback (<see cref="WheelGestureWatcher"/>), and it is the only
    /// window the app has, so closing it ends the process and takes the tray icon, the
    /// hotkeys and the dialog with it.
    /// </summary>
    private void ToggleStatus()
    {
        if (StatusView.Visibility == Visibility.Visible)
        {
            StatusView.Visibility = Visibility.Collapsed;
            AppWindow.Hide();
            return;
        }
        ShowStatus();
    }

    private async void ShowStatus()
    {
        StatusView.Visibility = Visibility.Visible;
        RepositionWindow(460, 280);
        StatusList.Children.Clear();
        StatusList.Children.Add(new TextBlock { Text = "查询中..." });
        AppWindow.Show();
        Activate();
        RenderStatus(await _runtime.StatusAsync());
    }

    // -- tray menu -----------------------------------------------------------

    private async void OnStartClick(object sender, RoutedEventArgs e)
    {
        TrayIcon.ToolTipText = "voice-core: 启动中...";
        var status = await _runtime.EnsureRunningAsync();
        _note = status.Detail ?? string.Empty;
        await RefreshTrayAsync();
    }

    private async void OnStopClick(object sender, RoutedEventArgs e)
    {
        await _runtime.StopAsync();
        await RefreshTrayAsync();
    }

    private void OnStatusClick(object sender, RoutedEventArgs e) => ToggleStatus();

    private async void OnWarmClick(object sender, RoutedEventArgs e)
    {
        Note("预热中…");
        Note(await _runtime.WarmAsync());
        await RefreshTrayAsync();
    }

    private async void OnSleepClick(object sender, RoutedEventArgs e)
    {
        Note(await _runtime.SleepAsync());
        await RefreshTrayAsync();
    }

    private void OnSubtitleToggle(object sender, RoutedEventArgs e) =>
        _presenter.SetEnabled(MenuSubtitles.IsChecked);

    private void OnPinToggle(object sender, RoutedEventArgs e) =>
        _presenter.Pinned = MenuPin.IsChecked;

    private void OnHistoryClick(object sender, RoutedEventArgs e) => _presenter.OpenHistory();

    private void OnResetSubtitlePositionClick(object sender, RoutedEventArgs e) =>
        _presenter.ResetPosition();

    private void OnAutoplayToggle(object sender, RoutedEventArgs e)
    {
        _autoplayEnabled = MenuAutoplay.IsChecked;
    }

    private void OnLogsClick(object sender, RoutedEventArgs e)
    {
        Directory.CreateDirectory(_runtime.LogDir);
        Process.Start(new ProcessStartInfo("explorer.exe", _runtime.LogDir));
    }

    /// <summary>
    /// One settings entry: everything a user may want to change lives in
    /// <c>config.json</c> - dialog, hotkeys and the voice pack registry - so 设置 opens that
    /// one file. The tray reads it at startup (a change needs a restart); the runtime
    /// re-reads the packs section by itself whenever the file's mtime changes.
    /// </summary>
    private void OnSettingsClick(object sender, RoutedEventArgs e)
    {
        // Touch it first so there is something to edit on a fresh install.
        AppConfig.Load(_runtime.DataDir, Note);
        Process.Start(new ProcessStartInfo("notepad.exe",
            Path.Combine(_runtime.DataDir, AppConfig.FileName)));
        Note("改完设置后重启托盘生效（声线包除外）。");
    }

    private async void OnExitClick(object sender, RoutedEventArgs e)
    {
        _wheel.Dispose();
        _hotkeys.Dispose();
        _presenter.Dispose();
        await _runtime.StopAsync();
        TrayIcon.Dispose();
        Application.Current.Exit();
    }
}
