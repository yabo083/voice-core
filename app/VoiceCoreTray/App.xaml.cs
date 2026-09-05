using Microsoft.UI.Xaml;
using System.Runtime.InteropServices;
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
        // Before anything else, including XAML init: the policy applies to the whole process
        // for its whole life, so declaring it first means startup itself is not scheduled as
        // background work. Same reason the engine worker declares it before importing torch.
        DeclinePowerThrottling();

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

    /// <summary>
    /// Tell Windows not to power-throttle this process.
    ///
    /// Measured on the engine worker, which is spawned the same way this is: with no stated
    /// policy Windows' heuristic treats a windowless child of a console process as background
    /// work and parks it on an E-core at reduced clock, which cost that process **3x**
    /// (4794 ms -> 1589 ms per utterance, and a 33% -> 3% spread). This process animates a
    /// subtitle at frame rate and drives audio playback, so the same treatment shows up as
    /// jank rather than as latency - the symptom nobody would have connected to scheduling.
    ///
    /// `VC_ENGINE_ECOQOS=1` keeps Windows' heuristic, the same switch the worker honours:
    /// declining the throttle is right for work a user is waiting on, but it is a tradeoff
    /// against battery and a tradeoff nobody can see is a bug.
    /// </summary>
    private static void DeclinePowerThrottling()
    {
        if (Environment.GetEnvironmentVariable("VC_ENGINE_ECOQOS") == "1")
        {
            Log("qos: VC_ENGINE_ECOQOS=1, leaving Windows' heuristic in charge");
            return;
        }
        try
        {
            var state = new PROCESS_POWER_THROTTLING_STATE
            {
                Version = 1, // PROCESS_POWER_THROTTLING_CURRENT_VERSION
                ControlMask = 0x1, // PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                StateMask = 0, // ... and the state we want for it: off
            };
            var size = Marshal.SizeOf<PROCESS_POWER_THROTTLING_STATE>();
            var buffer = Marshal.AllocHGlobal(size);
            try
            {
                Marshal.StructureToPtr(state, buffer, false);
                // ProcessPowerThrottling = 4. GetCurrentProcess() returns a pseudo-handle, so
                // there is nothing to close.
                var ok = SetProcessInformation(GetCurrentProcess(), 4, buffer, (uint)size);
                // Read it back rather than trusting the call: on a locked-down box this fails,
                // and a failure that logs like a success is worse than no log at all. The read
                // needs its own Version stamped into the buffer - a zeroed Version is not a
                // request for "current", it is an invalid one, and the call then answers with a
                // mask of 0 that is indistinguishable from "no policy stated". That is how the
                // first version of this line came to report `mask=0x0` on a process the OS had
                // in fact accepted the declination for.
                Marshal.StructureToPtr(new PROCESS_POWER_THROTTLING_STATE { Version = 1 }, buffer, false);
                var read = GetProcessInformation(GetCurrentProcess(), 4, buffer, (uint)size);
                var readErr = read ? 0 : Marshal.GetLastWin32Error();
                var got = read
                    ? Marshal.PtrToStructure<PROCESS_POWER_THROTTLING_STATE>(buffer)
                    : default;
                Log($"qos: asked=throttle-off set={ok} read={read} mask=0x{got.ControlMask:x} state=0x{got.StateMask:x}"
                    + (ok ? "" : $" set_err={Marshal.GetLastWin32Error()}")
                    + (read ? "" : $" read_err={readErr}"));
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }
        catch (Exception ex)
        {
            // Never fatal: a subtitle window that will not start because of a scheduling hint
            // would be a far worse bug than the jank this prevents.
            Log("qos: unavailable, continuing throttled: " + ex.Message);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PROCESS_POWER_THROTTLING_STATE
    {
        public uint Version;
        public uint ControlMask;
        public uint StateMask;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetProcessInformation(IntPtr process, int infoClass, IntPtr info, uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetProcessInformation(IntPtr process, int infoClass, IntPtr info, uint size);

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
            Dialog: null,
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
