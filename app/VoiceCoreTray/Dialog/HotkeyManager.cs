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
/// <see cref="AppConfig"/>) in the runtime's data dir and are re-applied whenever that
/// file changes (<see cref="Rebind"/>), so editing one takes effect where it was edited.
/// A failed registration - almost always another app already owns the combination - is
/// reported through the tray's status note instead of disappearing.
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
    private readonly string _dataDir;
    private readonly Action<string> _note;
    private readonly IReadOnlyDictionary<HotkeyAction, Action> _actions;
    private readonly Dictionary<int, Action> _handlers = new();
    /// <summary>The spec each action is CURRENTLY registered with. It is what lets a rebind
    /// tell an unchanged binding (leave it alone) from a changed one, and it is only ever
    /// written after <c>RegisterHotKey</c> succeeded - so it can never claim a key this
    /// window does not own.</summary>
    private readonly Dictionary<HotkeyAction, string> _bound = new();

    public HotkeyManager(nint hwnd, string dataDir, AppConfig.HotkeySection bindings,
        Action<string> note, IReadOnlyDictionary<HotkeyAction, Action> actions)
    {
        _hwnd = hwnd;
        _dataDir = dataDir;
        _note = note;
        _actions = actions;
        _messages = new WindowMessageMonitor(hwnd);
        _messages.WindowMessageReceived += OnWindowMessage;
        Rebind(bindings);
    }

    /// <summary>
    /// Apply the bindings as <c>config.json</c> now states them. Called at startup and again
    /// on every change to that file.
    ///
    /// An action whose spec did not change is left registered: dropping and re-taking the
    /// same combination would open a window in which the key belongs to nobody. One whose
    /// spec DID change is unregistered first, because <c>RegisterHotKey</c> refuses a
    /// combination this window already owns - which is how "the new binding silently does
    /// nothing while the old one still works" happens.
    ///
    /// A failure leaves that action UNBOUND and says so. Keeping the old registration would
    /// mean a live hotkey that no file agrees with, and the point of re-reading the file is
    /// that what it says is what is in effect.
    /// </summary>
    public void Rebind(AppConfig.HotkeySection bindings)
    {
        foreach (var (action, handler) in _actions)
        {
            var spec = Spec(bindings, action);
            if (_bound.TryGetValue(action, out var current) && current == spec) continue;

            int id = IdBase + (int)action;
            if (_bound.Remove(action))
            {
                UnregisterHotKey(_hwnd, id);
                _handlers.Remove(id);
            }

            if (!TryParse(spec, out uint modifiers, out uint key))
            {
                _note($"快捷键「{spec}」无法解析（{Label(action)}），现在没有绑定：请修正 {ConfigPath}");
                continue;
            }

            if (!RegisterHotKey(_hwnd, id, modifiers | MOD_NOREPEAT, key))
            {
                // A failure with its remedy, in one line: what broke, that the key is now
                // dead, and the file to fix it in. Whether a restart is needed is not part
                // of it - nothing here ever needed one.
                _note($"快捷键 {spec}（{Label(action)}）注册失败：可能已被其他程序占用，" +
                      $"现在没有绑定；改 {ConfigPath} 里的绑定即可。");
                continue;
            }

            _handlers[id] = handler;
            _bound[action] = spec;
        }
    }

    private static string Spec(AppConfig.HotkeySection bindings, HotkeyAction action) =>
        action == HotkeyAction.ToggleDialog ? bindings.ToggleDialog : bindings.ToggleHold;

    private string ConfigPath => Path.Combine(_dataDir, AppConfig.FileName);

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
        foreach (int id in _handlers.Keys) UnregisterHotKey(_hwnd, id);
        _handlers.Clear();
        _bound.Clear();
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
