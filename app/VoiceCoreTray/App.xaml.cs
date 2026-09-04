using Microsoft.UI.Xaml;
using System.Threading;
using VoiceCoreTray.Dialog;
using VoiceCoreTray.Services;

namespace VoiceCoreTray;

public partial class App : Application
{
    private static Mutex? _singleInstance;

    private readonly SubtitleOptions _options;
    private MainWindow? _window;
    private DialogPresenter? _probe;

    public App()
    {
        InitializeComponent();

        // Debug/behaviour flags (--subtitle-dwell / --subtitle-test / --subtitle-pin /
        // --subtitle-selftest / --no-runtime / --presenter). Unknown arguments are ignored,
        // which is what lets VoiceCore.exe pass both --presenter and --no-runtime without
        // caring which build it spawned.
        _options = SubtitleOptions.Parse(Environment.GetCommandLineArgs());

        // One mutex per ROLE, not per executable. A style probe must be able to run beside a
        // live tray, and the presenter VoiceCore.exe spawns must be able to run beside a
        // developer's standalone tray - a shared name would make one of the two exit at
        // startup with no output, which reads as a crash.
        var instanceName = _options.IsStyleProbe ? "voice-core-winui-tray-probe"
            : _options.PresenterMode ? "voice-core-presenter"
            : "voice-core-winui-tray";
        _singleInstance = new Mutex(true, instanceName, out var isNew);
        if (!isNew)
        {
            // Already running; another instance of this role owns the resources.
            Environment.Exit(0);
        }
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // The product promise is that nothing here ever puts a Windows "stopped working"
        // dialog in front of the user. Every tray menu handler is a void XAML handler and
        // every runtime event arrives on a dispatcher callback, so an unhandled throw from
        // any of them lands here: log it and mark it handled rather than taking the process
        // (and with it the tray icon, the hotkeys and the dialog) down.
        UnhandledException += (_, e) =>
        {
            Log("UNHANDLED " + e.Exception);
            e.Handled = true;
        };

        try
        {
            Log("OnLaunched enter");

            if (_options.IsStyleProbe)
            {
                RunStyleProbe();
                return;
            }

            // No window is shown at launch in either role: with a tray, the icon is the
            // product surface; under --presenter, the dialog is, and this window exists only
            // to own the hotkey registrations and the wheel hook.
            _window = new MainWindow(_options);
            Log("window created");
        }
        catch (Exception ex)
        {
            // A failure IN HERE leaves no tray icon, so there is no surface left to report
            // through and no point continuing: the log line is the whole diagnosis.
            Log("ERROR " + ex);
            throw;
        }
    }

    /// <summary>
    /// Dialog-only launch: no tray, no runtime, no hotkeys, so the dialog's styling
    /// and layout can be previewed (--subtitle-test) or measured (--subtitle-selftest)
    /// without the engine or a duplicate tray icon. Hotkeys stay out on purpose —
    /// the tray owns them, and a probe must not steal a live tray's registrations.
    /// </summary>
    private void RunStyleProbe()
    {
        var probe = new DialogPresenter(RuntimeClient.Default, _options);
        _probe = probe;

        if (_options.SelfTestReport is string report)
        {
            Log("selftest -> " + report);
            Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread().TryEnqueue(async () =>
            {
                try
                {
                    await probe.RunSelfTestAsync(report);
                    Log("selftest done");
                }
                catch (Exception ex) { Log("ERROR selftest: " + ex); }
                Environment.Exit(0);
            });
            return;
        }

        Log("style probe: runtime launch skipped");
        probe.Show(new DialogUtterance(
            AudioId: string.Empty,
            DisplayText: _options.TestText,
            Text: null,
            Wav: null,
            DisplaySeconds: null,
            Autoplay: false));
    }

    /// <summary>
    /// The one diagnosis channel that exists before any surface does, and the ONLY one under
    /// <c>--presenter</c>: that role has no tooltip and no status view, so
    /// <see cref="MainWindow"/> routes its notes here instead of into a window nobody can open.
    /// </summary>
    internal static void Log(string message) =>
        File.AppendAllText(Path.Combine(Path.GetTempPath(), "vc-tray-boot.log"),
            $"{DateTime.Now:HH:mm:ss.fff} {message}{Environment.NewLine}");
}
