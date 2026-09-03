using System.Diagnostics;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text.Json;

namespace VoiceCoreTray.Services;

/// <summary>
/// The tray's only link to voice-core. It is a presenter: it subscribes to the
/// runtime's event stream, fetches audio by id, and drives control actions
/// through the same public API the CLI and an agent use.
///
/// What it deliberately does NOT know: worker ports, model paths, Python
/// virtualenvs, voice pack formats, PID ledgers. v1's tray owned all of that,
/// which is why the product could not run without this GUI. Launching the
/// runtime needs no engine knowledge either — engine paths live in the
/// runtime's own <c>runtime.json</c>.
/// </summary>
public sealed class RuntimeClient : IAsyncDisposable
{
    public static RuntimeClient Default { get; } = new();

    private readonly HttpClient _http = new() { Timeout = TimeSpan.FromSeconds(10) };
    private readonly HttpClient _stream = new() { Timeout = Timeout.InfiniteTimeSpan };
    private readonly System.Collections.Concurrent.ConcurrentDictionary<string, Character> _characters = new();
    private CancellationTokenSource? _subscription;

    /// The runtime's bearer token, read from disk at most once per runtime lifetime.
    /// It has to be cached: `Request` is built before the first await of every call, so an
    /// uncached read put `token.txt` on the UI thread — the thread that owns the
    /// `WH_MOUSE_LL` hook — twelve times a minute, where a slow disk is a stuttering
    /// cursor across the whole desktop. Cleared on a 401/403, because a runtime restarted
    /// against a fresh data dir mints a new one and a cache that never expired would
    /// strand the tray.
    private string? _token;

    /// Set when the runtime announced its own exit, so the reconnect loop can tell an
    /// orderly shutdown from an outage. Touched only from the event-stream flow, whose
    /// awaits already order the write against the read.
    private bool _shutdownAnnounced;

    /// The reconnect ladder. 2 s keeps a runtime restart nearly invisible; the 30 s ceiling
    /// is what an idle tray costs once the engine is deliberately stopped — 2 wakes a
    /// minute beside the 12 status polls, instead of the 30 a fixed 2 s delay never stopped
    /// paying, on a product whose headline is ~0% CPU idle.
    private static readonly TimeSpan MinReconnectDelay = TimeSpan.FromSeconds(2);
    private static readonly TimeSpan MaxReconnectDelay = TimeSpan.FromSeconds(30);

    public string BaseUrl { get; }
    public string DataDir { get; }
    public string LogDir => Path.Combine(DataDir, "logs");

    /// <summary>Raised for each utterance the runtime produced, audio included.</summary>
    public event Action<SpeechEvent>? SpeechReceived;

    /// <summary>Raised for engine lifecycle and progress lines worth showing.</summary>
    public event Action<string>? StatusNote;

    public RuntimeClient()
    {
        BaseUrl = Environment.GetEnvironmentVariable("VC_URL")?.TrimEnd('/') ?? "http://127.0.0.1:8760";
        DataDir = ResolveDataDir();
    }

    /// <summary>One aligned segment: what the human reads, and the part of the spoken line
    /// that means it. Supplied by whoever called <c>speak</c>, because only the caller knows -
    /// Chinese and Japanese do not line up positionally, so a client that guesses renders a
    /// correspondence that is not there.</summary>
    public sealed record RubyPair(string Base, string Ruby);

    public sealed record SpeechEvent(
        string RequestId,
        string AudioId,
        string? DisplayText,
        string? Text,
        string? VoicePackId,
        double? DisplaySeconds,
        byte[]? Wav,
        IReadOnlyList<RubyPair>? RubyPairs);

    public sealed record RuntimeStatus(
        bool Reachable,
        bool EngineRunning,
        bool ModelLoaded,
        int VoicePacks,
        long UptimeMs,
        string? Detail,
        // The code the runtime answered with, or null when nothing answered on the port
        // at all. The two must not collapse: an answer of any code proves a runtime owns
        // 8760, and a second one could only fail to bind it and die.
        int? HttpStatus = null)
    {
        public string Summary => Reachable
            ? (EngineRunning
                ? (ModelLoaded ? "运行中（声线模型已加载）" : "运行中（模型未加载）")
                : "运行中（引擎空闲，显存已释放）")
            : HttpStatus is int code
                ? (code is >= 200 and < 300
                    ? $"runtime 运行中但状态无法解析（HTTP {code}）"
                    : $"runtime 运行中但拒绝请求（HTTP {code}）")
                : "runtime 未运行";
    }

    /// <summary>Presentation metadata for a voice pack. It travels with the pack,
    /// because the pack is what knows whose voice it is.</summary>
    public sealed record Character(string Name, string? AvatarPath);

    /// <summary>
    /// Speaker name and portrait for a voice pack id, or null when the pack carries
    /// neither. Cached: `GET /api/voices` is cheap but a speech event is not the
    /// moment to make a round trip, and the runtime reloads the registry itself when
    /// the file changes.
    /// </summary>
    public async Task<Character?> CharacterAsync(string? voicePackId, CancellationToken ct = default)
    {
        if (string.IsNullOrEmpty(voicePackId)) return null;
        if (_characters.TryGetValue(voicePackId, out var cached)) return cached;

        try
        {
            using var request = Request(HttpMethod.Get, "/api/voices");
            using var response = await _http.SendAsync(request, ct);
            if (!response.IsSuccessStatusCode) return null;
            using var document = JsonDocument.Parse(await response.Content.ReadAsStringAsync(ct));
            foreach (var pack in document.RootElement.EnumerateArray())
            {
                var id = Read(pack, "id");
                if (id is null) continue;
                var name = Read(pack, "character") ?? Read(pack, "name") ?? id;
                var avatar = Read(pack, "avatar");
                _characters[id] = new Character(name, ResolveAvatar(avatar));
            }
        }
        catch { return null; }

        return _characters.TryGetValue(voicePackId, out var found) ? found : null;
    }

    /// <summary>Pack-relative avatar paths resolve against the data dir, exactly like
    /// the pack payload itself, so an install stays portable.</summary>
    private string? ResolveAvatar(string? avatar)
    {
        if (string.IsNullOrWhiteSpace(avatar)) return null;
        var path = Path.IsPathRooted(avatar) ? avatar : Path.Combine(DataDir, avatar);
        return File.Exists(path) ? path : null;
    }

    // -- discovery -----------------------------------------------------------

    /// dist layout: &lt;root&gt;\bin\app\VoiceCoreTray.exe with &lt;root&gt;\data;
    /// dev layout: the repo's own data dir beside Cargo.toml.
    private static string ResolveDataDir()
    {
        var fromEnv = Environment.GetEnvironmentVariable("VC_DATA_DIR");
        if (!string.IsNullOrWhiteSpace(fromEnv)) return fromEnv;

        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        // A Release build sits seven levels below the repo root
        // (app/VoiceCoreTray/bin/x64/Release/net8.0-.../), so the walk must be
        // deep enough to reach it as well as the shallower dist layout.
        for (int i = 0; i < 8 && dir is not null; i++, dir = dir.Parent)
        {
            if (File.Exists(Path.Combine(dir.FullName, "Cargo.toml")) ||
                File.Exists(Path.Combine(dir.FullName, "bin", "voice-core-runtime.exe")))
            {
                return DataDirUnder(dir.FullName);
            }
        }
        return DataDirUnder(AppContext.BaseDirectory);
    }

    /// The runtime's rule, and it has to stay the runtime's rule (<c>resolve_data_dir</c>,
    /// src/bin/voice-core-runtime.rs:325-334): <c>&lt;root&gt;\data</c> unless that
    /// directory refuses writes, in which case both processes move to
    /// <c>%APPDATA%\voice-core</c>. A tray that answered this differently read a token, a
    /// config and a log directory the runtime never touched — every request a 401 against
    /// a runtime that was running fine.
    private static string DataDirUnder(string root)
    {
        var preferred = Path.Combine(root, "data");
        if (IsWritable(preferred)) return preferred;
        var appData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        // Empty means the folder could not be resolved, which is where the runtime finds no
        // APPDATA and keeps the unwritable choice; combining onto "" would hand back a
        // RELATIVE path, and a tray launched from a shortcut has an arbitrary cwd.
        return appData.Length > 0 ? Path.Combine(appData, "voice-core") : preferred;
    }

    /// The runtime's probe, kept identical (<c>is_writable</c>,
    /// src/bin/voice-core-runtime.rs:336-348): only a failed create-or-write means
    /// unwritable. A probe file that cannot be deleted afterwards is still proof the
    /// directory takes writes, and calling it unwritable would split the two processes
    /// apart over a lock an antivirus scan holds for a moment.
    private static bool IsWritable(string dir)
    {
        var probe = string.Empty;
        try
        {
            Directory.CreateDirectory(dir);
            probe = Path.Combine(dir, ".write-probe");
            File.WriteAllBytes(probe, Array.Empty<byte>());
        }
        catch { return false; }
        try { File.Delete(probe); }
        catch { /* the write already answered the question */ }
        return true;
    }

    private string? FindRuntimeExe()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        for (int i = 0; i < 8 && dir is not null; i++, dir = dir.Parent)
        {
            foreach (var relative in new[]
            {
                Path.Combine("bin", "voice-core-runtime.exe"),
                Path.Combine("target", "release", "voice-core-runtime.exe"),
                Path.Combine("target", "debug", "voice-core-runtime.exe"),
            })
            {
                var candidate = Path.Combine(dir.FullName, relative);
                if (File.Exists(candidate)) return candidate;
            }
        }
        return null;
    }

    private string? Token()
    {
        if (_token is not null) return _token;
        try
        {
            var file = Path.Combine(DataDir, "token.txt");
            return _token = File.Exists(file) ? File.ReadAllText(file).Trim() : null;
        }
        catch { return null; }
    }

    private HttpRequestMessage Request(HttpMethod method, string path)
    {
        var request = new HttpRequestMessage(method, BaseUrl + path);
        if (Token() is string token)
        {
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
        }
        return request;
    }

    // -- lifecycle -----------------------------------------------------------

    /// <summary>Start the runtime if it is not already answering. One process,
    /// no arguments: everything it needs is in its own data dir.</summary>
    public async Task<RuntimeStatus> EnsureRunningAsync()
    {
        var status = await StatusAsync();
        if (status.Reachable) return status;
        // Something on this port is already speaking HTTP. Starting a second runtime
        // cannot help, and the real fault — usually a token from another data dir —
        // would stay hidden behind "未运行" for every retry.
        if (status.HttpStatus is not null) return status;

        var exe = FindRuntimeExe();
        if (exe is null)
        {
            return new RuntimeStatus(false, false, false, 0, 0,
                "找不到 voice-core-runtime.exe（在 bin\\ 或 target\\release\\ 下查找）");
        }

        var psi = new ProcessStartInfo
        {
            FileName = exe,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        // Forwarded ONLY when the user made the choice: pushing the tray's own answer onto
        // the child overrode the runtime's %APPDATA% fallback and pinned it to the very
        // directory it had just found unwritable, which is where "20 秒内未就绪" came from.
        var explicitDataDir = Environment.GetEnvironmentVariable("VC_DATA_DIR");
        if (!string.IsNullOrWhiteSpace(explicitDataDir))
        {
            psi.ArgumentList.Add("--data-dir");
            psi.ArgumentList.Add(DataDir);
        }

        try
        {
            // No pipe redirection on purpose: the runtime writes its own
            // logs/runtime.{out,err}.log, so it does not depend on this GUI
            // staying alive to pump its output, and a tray crash leaves the
            // service running.
            Process.Start(psi);
        }
        catch (Exception ex)
        {
            return new RuntimeStatus(false, false, false, 0, 0, "启动失败：" + ex.Message);
        }

        var deadline = DateTime.UtcNow.AddSeconds(20);
        while (DateTime.UtcNow < deadline)
        {
            await Task.Delay(300);
            status = await StatusAsync();
            if (status.Reachable || status.HttpStatus is not null)
            {
                // The runtime that announced its exit parked the event loop at the
                // ceiling; a user starting one again must not wait that out.
                StartSubscription();
                return status;
            }
        }
        return new RuntimeStatus(false, false, false, 0, 0, "runtime 启动后 20 秒内未就绪，见 logs\\runtime.err.log");
    }

    /// <summary>Ask the runtime to exit. It terminates its own engine tree.</summary>
    public async Task StopAsync()
    {
        try
        {
            using var request = Request(HttpMethod.Post, "/api/shutdown");
            using var _ = await _http.SendAsync(request);
        }
        catch { /* already gone */ }
    }

    public async Task<RuntimeStatus> StatusAsync()
    {
        int? httpStatus = null;
        try
        {
            using var request = Request(HttpMethod.Get, "/api/status");
            using var response = await _http.SendAsync(request);
            httpStatus = (int)response.StatusCode;
            if (!response.IsSuccessStatusCode)
            {
                return new RuntimeStatus(false, false, false, 0, 0,
                    await RefusedDetailAsync(httpStatus.Value), httpStatus);
            }
            using var document = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
            var root = document.RootElement;
            var worker = root.GetProperty("worker");
            return new RuntimeStatus(
                Reachable: true,
                EngineRunning: worker.GetProperty("running").GetBoolean(),
                ModelLoaded: worker.GetProperty("modelLoaded").GetBoolean(),
                VoicePacks: root.GetProperty("voicePacks").GetInt32(),
                UptimeMs: root.GetProperty("uptimeMs").GetInt64(),
                Detail: null,
                HttpStatus: httpStatus);
        }
        catch (Exception ex)
        {
            // A code already in hand means the runtime answered and it was the payload
            // that failed us — version skew, not an absent runtime.
            return new RuntimeStatus(false, false, false, 0, 0, ex.Message, httpStatus);
        }
    }

    /// <summary>Why a live runtime refused us. A rejected token is nearly always the tray
    /// and the runtime resolving different data dirs, and the file the tray reads is the
    /// one thing the user can check; <c>/api/health</c> is the only route that needs no
    /// token, so an answer there beside a rejected <c>/api/status</c> is that signature.</summary>
    private async Task<string> RefusedDetailAsync(int code)
    {
        if (code is not 401 and not 403) return $"HTTP {code}";
        // The one place the cache can be wrong: a runtime restarted against a fresh data
        // dir minted a new token. Drop it here, on the answer that proves it, so the next
        // request re-reads — a timer would either poll the disk or expire it too late.
        _token = null;
        var token = Path.Combine(DataDir, "token.txt");
        var live = await HealthAnswersAsync();
        return live
            ? $"HTTP {code}：runtime 在运行但拒绝了令牌，托盘与 runtime 的数据目录不一致；核对 {token}"
            : $"HTTP {code}：令牌被拒绝，核对 {token}";
    }

    private async Task<bool> HealthAnswersAsync()
    {
        try
        {
            // Asked without a token on purpose: /api/health is unauthenticated.
            using var response = await _http.GetAsync(BaseUrl + "/api/health");
            return response.IsSuccessStatusCode;
        }
        catch { return false; }
    }

    /// <summary>Load the model now so a conversation does not pay for it.</summary>
    public async Task<string> WarmAsync()
    {
        try
        {
            using var request = Request(HttpMethod.Post, "/api/warm");
            // Disposed: an undisposed source leaves its three-minute timer on the queue long
            // after /api/warm answered, waking a process whose whole point is to sit at zero.
            using var timeout = new CancellationTokenSource(TimeSpan.FromMinutes(3));
            using var response = await _http.SendAsync(request, HttpCompletionOption.ResponseContentRead,
                timeout.Token);
            return response.IsSuccessStatusCode ? "声线模型已加载" : await DescribeErrorAsync(response);
        }
        catch (Exception ex) { return "预热失败：" + ex.Message; }
    }

    /// <summary>Release GPU memory without stopping the runtime.</summary>
    public async Task<string> SleepAsync()
    {
        try
        {
            using var request = Request(HttpMethod.Post, "/api/sleep");
            using var response = await _http.SendAsync(request);
            return response.IsSuccessStatusCode ? "引擎已停止，显存已释放" : await DescribeErrorAsync(response);
        }
        catch (Exception ex) { return "释放失败：" + ex.Message; }
    }

    /// <summary>Structured errors stay structured: show the code and the action.</summary>
    private static async Task<string> DescribeErrorAsync(HttpResponseMessage response)
    {
        try
        {
            using var document = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
            var root = document.RootElement;
            var message = $"[{root.GetProperty("code").GetString()}] {root.GetProperty("message").GetString()}";
            if (root.TryGetProperty("recovery", out var recovery) &&
                recovery.TryGetProperty("detail", out var detail))
            {
                message += " — " + detail.GetString();
            }
            return message;
        }
        catch { return $"HTTP {(int)response.StatusCode}"; }
    }

    // -- event stream --------------------------------------------------------

    /// <summary>
    /// Subscribe to <c>GET /api/events</c> and keep re-subscribing. This replaces
    /// v1's inbound push server: the runtime no longer needs to know a GUI
    /// exists, let alone which port it listens on.
    /// </summary>
    public void StartSubscription()
    {
        _subscription?.Cancel();
        _subscription = new CancellationTokenSource();
        _ = SubscribeLoopAsync(_subscription.Token);
    }

    private async Task SubscribeLoopAsync(CancellationToken ct)
    {
        var backoff = MinReconnectDelay;
        while (!ct.IsCancellationRequested)
        {
            try
            {
                await SubscribeOnceAsync(ct);
                backoff = MinReconnectDelay;
            }
            catch (OperationCanceledException) { return; }
            catch { /* runtime down or restarting */ }
            // An announced exit is not an outage: settle at the ceiling at once rather than
            // climb the ladder against a process the user already told to go away.
            if (_shutdownAnnounced)
            {
                _shutdownAnnounced = false;
                backoff = MaxReconnectDelay;
            }
            try { await Task.Delay(backoff, ct); }
            catch (OperationCanceledException) { return; }
            if (backoff < MaxReconnectDelay)
            {
                backoff = TimeSpan.FromSeconds(
                    Math.Min(MaxReconnectDelay.TotalSeconds, backoff.TotalSeconds * 2));
            }
        }
    }

    private async Task SubscribeOnceAsync(CancellationToken ct)
    {
        using var request = Request(HttpMethod.Get, "/api/events");
        using var response = await _stream.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct);
        response.EnsureSuccessStatusCode();

        using var stream = await response.Content.ReadAsStreamAsync(ct);
        using var reader = new StreamReader(stream);
        while (!ct.IsCancellationRequested && await reader.ReadLineAsync(ct) is string line)
        {
            if (!line.StartsWith("data:", StringComparison.Ordinal)) continue;
            var payload = line[5..].Trim();
            if (payload.Length == 0) continue;
            await HandleEventAsync(payload, ct);
        }
    }

    private async Task HandleEventAsync(string payload, CancellationToken ct)
    {
        string? kind;
        JsonDocument document;
        try
        {
            document = JsonDocument.Parse(payload);
            kind = document.RootElement.TryGetProperty("kind", out var k) ? k.GetString() : null;
        }
        catch { return; }

        using (document)
        {
            var root = document.RootElement;
            switch (kind)
            {
                case "speech":
                {
                    var audioId = root.GetProperty("audioId").GetString() ?? "";
                    var wav = await FetchAudioAsync(audioId, ct);
                    if (wav is null && audioId.Length > 0)
                    {
                        StatusNote?.Invoke($"语音取回失败（audioId={audioId}），本句只有字幕");
                    }
                    SpeechReceived?.Invoke(new SpeechEvent(
                        RequestId: root.TryGetProperty("requestId", out var r) ? r.GetString() ?? "" : "",
                        AudioId: audioId,
                        DisplayText: Read(root, "displayText"),
                        Text: Read(root, "text"),
                        VoicePackId: Read(root, "voicePackId"),
                        DisplaySeconds: root.TryGetProperty("displaySeconds", out var s) &&
                                        s.ValueKind == JsonValueKind.Number
                            ? s.GetDouble()
                            : null,
                        Wav: wav,
                        RubyPairs: ReadRubyPairs(root)));
                    break;
                }
                case "workerStarting":
                    StatusNote?.Invoke("引擎启动中…");
                    break;
                case "workerReady":
                    StatusNote?.Invoke("引擎就绪");
                    break;
                case "workerStopped":
                    StatusNote?.Invoke("引擎已停止：" + (Read(root, "reason") ?? ""));
                    break;
                case "progress":
                    if (Read(root, "message") is string message) StatusNote?.Invoke(message);
                    break;
                case "speakFailed":
                    StatusNote?.Invoke($"合成失败 [{Read(root, "code")}] {Read(root, "message")}");
                    break;
                case "runtimeStopping":
                    // The stream is about to end by design, not by failure: re-dialling a
                    // process that just said goodbye is the outage the tray invents itself.
                    _shutdownAnnounced = true;
                    StatusNote?.Invoke("runtime 正在退出");
                    break;
                case "runtimeReady":
                    StatusNote?.Invoke("runtime 服务就绪");
                    break;
                case "speakStarted":
                    // Deliberately silent: it precedes every `speech` by a synthesis, and a
                    // note here would overwrite the line the user is about to be shown.
                    break;
                // src/obs.rs emits nine kinds and will add more; without this arm the next
                // one is read off the wire and dropped with nothing anywhere saying so.
                default:
                    StatusNote?.Invoke($"收到未知事件：{kind ?? "unknown"}");
                    break;
            }
        }
    }

    private static string? Read(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    /// <summary>
    /// Read the caller's segment alignment, or null when it sent none. Malformed entries are
    /// skipped rather than thrown: the alignment is an enhancement, and a bad one must not
    /// cost the utterance itself.
    /// </summary>
    private static IReadOnlyList<RubyPair>? ReadRubyPairs(JsonElement root)
    {
        if (!root.TryGetProperty("rubyPairs", out var array) || array.ValueKind != JsonValueKind.Array)
            return null;

        var pairs = new List<RubyPair>(array.GetArrayLength());
        foreach (var item in array.EnumerateArray())
        {
            if (item.ValueKind != JsonValueKind.Object) continue;
            if (Read(item, "base") is not string based || based.Length == 0) continue;
            pairs.Add(new RubyPair(based, Read(item, "ruby") ?? string.Empty));
        }
        return pairs.Count > 0 ? pairs : null;
    }

    /// <summary>
    /// Audio arrives as bytes from its own endpoint, never inside JSON. Public so
    /// a log/history view can replay an older utterance by id.
    /// </summary>
    public async Task<byte[]?> FetchAudioAsync(string audioId, CancellationToken ct = default)
    {
        if (audioId.Length == 0) return null;
        try
        {
            using var request = Request(HttpMethod.Get, "/api/audio/" + audioId);
            using var response = await _http.SendAsync(request, ct);
            return response.IsSuccessStatusCode ? await response.Content.ReadAsByteArrayAsync(ct) : null;
        }
        catch { return null; }
    }

    public async ValueTask DisposeAsync()
    {
        _subscription?.Cancel();
        _http.Dispose();
        _stream.Dispose();
        await Task.CompletedTask;
    }
}
