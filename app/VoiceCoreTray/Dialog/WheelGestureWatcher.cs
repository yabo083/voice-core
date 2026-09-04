using System.Runtime.InteropServices;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// A low-level mouse hook, owned by the tray, that turns "wheel over the dialog"
/// into a gesture.
///
/// This cannot be a XAML pointer event: <c>WM_MOUSEWHEEL</c> goes to the FOCUSED
/// window, and the dialog is <c>WS_EX_NOACTIVATE</c> on purpose - it must never take
/// focus from whatever the user is actually working in. A low-level hook is the only
/// way to see a wheel event aimed at a window that never gets focus.
///
/// The callback runs on the thread that installed it (the UI thread), so it does the
/// cheapest possible thing: everything that is not a wheel message is passed through
/// before any marshalling happens.
/// </summary>
internal sealed class WheelGestureWatcher : IDisposable
{
    private const int WH_MOUSE_LL = 14;
    private const int WM_MOUSEWHEEL = 0x020A;

    private delegate nint HookProc(int code, nint wParam, nint lParam);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern nint SetWindowsHookExW(int idHook, HookProc callback, nint module, uint threadId);
    [DllImport("user32.dll")]
    private static extern bool UnhookWindowsHookEx(nint hook);
    [DllImport("user32.dll")]
    private static extern nint CallNextHookEx(nint hook, int code, nint wParam, nint lParam);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern nint GetModuleHandleW(string? name);

    [StructLayout(LayoutKind.Sequential)]
    private struct MSLLHOOKSTRUCT
    {
        public int X;
        public int Y;
        public uint MouseData;
        public uint Flags;
        public uint Time;
        public nuint ExtraInfo;
    }

    /// <summary>Held in a field: a collected delegate means a crash inside the hook.</summary>
    private readonly HookProc _callback;
    private readonly Func<int, int, int, bool> _onWheel;
    private nint _hook;

    /// <param name="onWheel">Screen x, screen y, wheel delta. Return true to consume
    /// the event, which is right when the gesture belongs to the topmost overlay. It MUST
    /// return in microseconds: the raw input thread waits for this callback before
    /// delivering the event to ANY application, and a low-level hook whose callback
    /// exceeds LowLevelHooksTimeout (300 ms) is silently removed by Windows, after which
    /// the gesture is dead for the session with nothing reported.</param>
    /// <param name="note">Where a failed installation is reported. Staying silent here
    /// makes a missing gesture look like a broken dialog.</param>
    public WheelGestureWatcher(Func<int, int, int, bool> onWheel, Action<string> note)
    {
        _onWheel = onWheel;
        _callback = OnHook;
        _hook = SetWindowsHookExW(WH_MOUSE_LL, _callback, GetModuleHandleW(null), 0);
        if (_hook == 0)
            note($"鼠标滚轮手势注册失败（错误 {Marshal.GetLastWin32Error()}）：" +
                 "历史对话请使用对话框上的 历史 控件。");
    }

    private nint OnHook(int code, nint wParam, nint lParam)
    {
        if (code >= 0 && (uint)wParam == WM_MOUSEWHEEL)
        {
            var mouse = Marshal.PtrToStructure<MSLLHOOKSTRUCT>(lParam);
            int delta = unchecked((short)(mouse.MouseData >> 16));
            try
            {
                if (_onWheel(mouse.X, mouse.Y, delta)) return 1;
            }
            catch { /* a hook that throws takes the whole input queue down with it */ }
        }
        return CallNextHookEx(_hook, code, wParam, lParam);
    }

    public void Dispose()
    {
        if (_hook == 0) return;
        UnhookWindowsHookEx(_hook);
        _hook = 0;
    }
}
