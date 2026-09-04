using System.Runtime.InteropServices;
using Windows.System;
using WinUIEx.Messaging;

namespace VoiceCoreTray.Dialog;

/// <summary>What a hotkey does. The tray owns these; the dialog never sees a key.
/// Two, on purpose: the backlog has a control on the dialog and the wheel gesture, so a
/// third binding would be a key to remember for something already one gesture away.</summary>
internal enum HotkeyAction
{
    /// <summary>Hide the dialog, or bring the last utterance back.</summary>
    ToggleDialog,
    /// <summary>常驻 (stay until dismissed) &lt;-&gt; 倒计时 (auto-hide).</summary>
    ToggleHold,
}

/// <summary>
/// Global hotkeys, registered on the TRAY window.
///
/// <c>RegisterHotKey</c> needs a window that owns a message pump and that outlives
/// every transient surface; the dialog is neither (it is hidden most of the time and
/// deliberately never activates). Bindings come from <c>config.json</c> (see
/// <see cref="AppConfig"/>) in the runtime's data dir, and a
/// failed registration - almost always another app already owns the combination -
/// is reported through the tray's status note instead of disappearing.
/// </summary>
internal sealed class HotkeyManager : IDisposable
{
    private const uint WM_HOTKEY = 0x0312;
    private const uint MOD_ALT = 0x0001;
    private const uint MOD_CONTROL = 0x0002;
    private const uint MOD_SHIFT = 0x0004;
    private const uint MOD_WIN = 0x0008;
    private const uint MOD_NOREPEAT = 0x4000;
    private const int IdBase = 0xB000;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool RegisterHotKey(nint hWnd, int id, uint modifiers, uint virtualKey);
    [DllImport("user32.dll")]
    private static extern bool UnregisterHotKey(nint hWnd, int id);

    private readonly nint _hwnd;
    private readonly WindowMessageMonitor _messages;
    private readonly Dictionary<int, Action> _handlers = new();
    private readonly List<int> _registered = new();

    public HotkeyManager(nint hwnd, string dataDir, AppConfig.HotkeySection bindings,
        Action<string> note, IReadOnlyDictionary<HotkeyAction, Action> actions)
    {
        _hwnd = hwnd;
        _messages = new WindowMessageMonitor(hwnd);
        _messages.WindowMessageReceived += OnWindowMessage;

        foreach (var (action, handler) in actions)
        {
            var spec = action == HotkeyAction.ToggleDialog
                ? bindings.ToggleDialog
                : bindings.ToggleHold;
            if (!TryParse(spec, out uint modifiers, out uint key))
            {
                note($"快捷键「{spec}」无法解析（{Label(action)}），已跳过：请修正 {Path.Combine(dataDir, AppConfig.FileName)}");
                continue;
            }

            int id = IdBase + (int)action;
            if (!RegisterHotKey(_hwnd, id, modifiers | MOD_NOREPEAT, key))
            {
                note($"快捷键 {spec}（{Label(action)}）注册失败：可能已被其他程序占用，" +
                     $"可修改 {Path.Combine(dataDir, AppConfig.FileName)} 后重启 voice-core。");
                continue;
            }

            _handlers[id] = handler;
            _registered.Add(id);
        }
    }

    private void OnWindowMessage(object? sender, WindowMessageEventArgs e)
    {
        if (e.Message.MessageId != WM_HOTKEY) return;
        if (_handlers.TryGetValue((int)e.Message.WParam, out var handler))
        {
            handler();
            e.Result = 0;
            e.Handled = true;
        }
    }

    public void Dispose()
    {
        foreach (int id in _registered) UnregisterHotKey(_hwnd, id);
        _registered.Clear();
        _messages.Dispose();
    }

    // -- configuration -------------------------------------------------------
    //
    // The bindings arrive from AppConfig (config.json): one settings file for the whole
    // app, so a user hunting for a hotkey has exactly one place to look.

    private static string Label(HotkeyAction action) => action switch
    {
        HotkeyAction.ToggleDialog => "显示/隐藏对话框",
        _ => "切换常驻/倒计时",
    };

    /// <summary>
    /// Parse "Ctrl+Alt+D" style specs. At least one modifier is required: a bare
    /// key registered globally would swallow it from every other application.
    /// </summary>
    private static bool TryParse(string spec, out uint modifiers, out uint virtualKey)
    {
        modifiers = 0;
        virtualKey = 0;
        foreach (var token in spec.Split('+', StringSplitOptions.RemoveEmptyEntries |
                                              StringSplitOptions.TrimEntries))
        {
            switch (token.ToLowerInvariant())
            {
                case "ctrl":
                case "control": modifiers |= MOD_CONTROL; break;
                case "alt": modifiers |= MOD_ALT; break;
                case "shift": modifiers |= MOD_SHIFT; break;
                case "win":
                case "windows": modifiers |= MOD_WIN; break;
                default:
                    if (virtualKey != 0 || !TryKey(token, out virtualKey)) return false;
                    break;
            }
        }
        return modifiers != 0 && virtualKey != 0;
    }

    private static bool TryKey(string token, out uint virtualKey)
    {
        if (token.Length == 1)
        {
            char c = char.ToUpperInvariant(token[0]);
            if (c is >= 'A' and <= 'Z' or >= '0' and <= '9')
            {
                virtualKey = c;
                return true;
            }
        }
        // Covers F1..F24, Space, Escape, Tab, arrows, Home/End, PageUp/PageDown, Insert, Delete.
        if (Enum.TryParse<VirtualKey>(token, ignoreCase: true, out var key))
        {
            virtualKey = (uint)key;
            return true;
        }
        virtualKey = 0;
        return false;
    }
}
