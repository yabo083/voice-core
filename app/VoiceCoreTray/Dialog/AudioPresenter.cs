using Microsoft.UI.Dispatching;
using System.Buffers.Binary;
using System.Media;
using VoiceCoreTray.Services;

namespace VoiceCoreTray.Dialog;

/// <summary>
/// Playback of the WAV bytes the tray already fetched, plus replay of an older
/// utterance by spool id.
///
/// Still <see cref="SoundPlayer"/>: the payload is always a plain PCM WAV from the
/// runtime, playback is fire-and-forget, and the alternative (a mixer such as NAudio)
/// buys volume control that no runtime route can request and costs a dependency.
///
/// Three things here are about not stalling the UI thread, which matters more than it
/// looks: the tray owns a <c>WH_MOUSE_LL</c> hook on that thread, so a block there
/// delays mouse input for the whole desktop.
///   * <c>PlaySync</c> runs on a worker and completion is marshalled back.
///   * Stopping the previous clip also happens on that worker. <c>SoundPlayer.Stop</c>
///     talks to winmm and can block; doing it inline made a dismissal (or a rapid
///     replay) hitch the cursor.
///   * A replay is single-flight and its bytes are cached. Repeated clicks used to mean
///     one HTTP GET of a multi-megabyte WAV and one new player EACH, which is how a
///     double-click turned into thread-pool churn.
/// </summary>
internal sealed class AudioPresenter(DispatcherQueue dispatcher, RuntimeClient runtime,
    DialogMetrics metrics)
{
    /// <summary>Outcome of a replay attempt, so the caller can say which happened.</summary>
    internal enum Replay
    {
        /// <summary>Playing now.</summary>
        Started,
        /// <summary>The spool no longer has it (TTL, byte cap, or a runtime restart).</summary>
        Missing,
        /// <summary>A replay was already in flight; this one was dropped on purpose.</summary>
        Busy,
    }

    /// <summary>Last few replayed clips, so holding a line and pressing again is free.
    /// Bounded by count, not bytes: three PCM clips of a spoken sentence is single-digit
    /// megabytes, and the spool is the real store.</summary>
    private const int CacheDepth = 3;

    private readonly object _gate = new();
    private readonly Dictionary<string, byte[]> _cache = new();
    private readonly Queue<string> _cacheOrder = new();
    private SoundPlayer? _player;
    private int _generation;
    private int _replayInFlight;

    /// <summary>
    /// Start playing <paramref name="wav"/>, replacing anything already playing.
    /// Returns the clip length in seconds when the RIFF header is readable, which is how
    /// the text pacing learns how long the voice will take (the runtime's speech event
    /// carries <c>durationMs</c>, but the tray's SpeechEvent record does not expose it).
    ///
    /// Nothing here touches the audio device on the calling thread: both stopping the
    /// previous clip and playing the new one happen on one worker.
    /// </summary>
    public double? Play(byte[] wav, Action? onFinished)
    {
        double? seconds = DurationSeconds(wav);
        SoundPlayer player;
        SoundPlayer? previous;
        int generation;
        lock (_gate)
        {
            generation = ++_generation;
            previous = _player;
            player = new SoundPlayer(new MemoryStream(wav, writable: false));
            _player = player;
        }

        _ = Task.Run(() =>
        {
            try { previous?.Stop(); }
            catch { /* already gone; its own worker will dispose it */ }

            try { player.PlaySync(); }
            catch { /* device busy, missing, or stopped from under us */ }
            finally
            {
                lock (_gate)
                {
                    if (ReferenceEquals(_player, player)) _player = null;
                }
                player.Dispose();
            }

            if (onFinished is not null)
            {
                dispatcher.TryEnqueue(() =>
                {
                    // A newer utterance already took over: its own completion wins.
                    if (Volatile.Read(ref _generation) == generation) onFinished();
                });
            }
        });

        return seconds;
    }

    /// <summary>
    /// Replay a backlog utterance. Single-flight: while one replay is being fetched or
    /// started, further attempts return <see cref="Replay.Busy"/> instead of queueing a
    /// download and a player each. Bytes are cached, so pressing the same line again
    /// costs nothing over the wire.
    /// </summary>
    public async Task<Replay> ReplayAsync(string audioId)
    {
        if (Interlocked.CompareExchange(ref _replayInFlight, 1, 0) != 0)
        {
            metrics.ReplaysSkipped++;
            return Replay.Busy;
        }

        try
        {
            byte[]? wav;
            lock (_gate) _cache.TryGetValue(audioId, out wav);

            if (wav is null)
            {
                metrics.ReplayFetches++;
                wav = await runtime.FetchAudioAsync(audioId);
                if (wav is null || wav.Length == 0) return Replay.Missing;
                Remember(audioId, wav);
            }
            else
            {
                metrics.ReplayCacheHits++;
            }

            Play(wav, null);
            return Replay.Started;
        }
        finally
        {
            Interlocked.Exchange(ref _replayInFlight, 0);
        }
    }

    private void Remember(string audioId, byte[] wav)
    {
        lock (_gate)
        {
            if (!_cache.TryAdd(audioId, wav)) return;
            _cacheOrder.Enqueue(audioId);
            while (_cacheOrder.Count > CacheDepth) _cache.Remove(_cacheOrder.Dequeue());
        }
    }

    /// <summary>Silence now. Safe to call when nothing is playing. The device call goes
    /// to a worker: <c>Stop</c> can block, and this runs on the UI thread.</summary>
    public void Stop()
    {
        SoundPlayer? player;
        lock (_gate)
        {
            _generation++;
            player = _player;
            _player = null;
        }
        if (player is null) return;

        _ = Task.Run(() =>
        {
            try { player.Stop(); }
            catch { /* already disposed by its own worker */ }
        });
    }

    /// <summary>
    /// Clip length from the RIFF header: <c>data</c> chunk size over <c>fmt </c>
    /// byte rate. Null when the header is not a WAV we understand, in which case
    /// the caller falls back to per-character pacing.
    /// </summary>
    private static double? DurationSeconds(ReadOnlySpan<byte> wav)
    {
        if (wav.Length < 44 || !wav[..4].SequenceEqual("RIFF"u8) || !wav[8..12].SequenceEqual("WAVE"u8))
            return null;

        uint byteRate = 0;
        int pos = 12;
        while (pos + 8 <= wav.Length)
        {
            var id = wav[pos..(pos + 4)];
            uint size = BinaryPrimitives.ReadUInt32LittleEndian(wav[(pos + 4)..(pos + 8)]);
            int body = pos + 8;
            if (size > int.MaxValue) return null;

            if (id.SequenceEqual("fmt "u8) && size >= 16 && body + 16 <= wav.Length)
            {
                byteRate = BinaryPrimitives.ReadUInt32LittleEndian(wav[(body + 8)..(body + 12)]);
            }
            else if (id.SequenceEqual("data"u8))
            {
                if (byteRate == 0) return null;
                long bytes = Math.Min(size, (uint)(wav.Length - body));
                return bytes / (double)byteRate;
            }

            // 64-bit arithmetic and a bounds check, not `pos + (int)size`: a corrupt size
            // near int.MaxValue overflows to a negative pos, `pos + 8 <= wav.Length` is still
            // true, and the next slice throws out of a UI-thread callback with nothing to
            // catch it. One bad spool payload must not take the tray down.
            long next = (long)body + size + (size & 1);   // chunks are word aligned
            if (next > wav.Length) return null;
            pos = (int)next;
        }
        return null;
    }
}
