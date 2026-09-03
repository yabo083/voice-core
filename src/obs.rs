//! Observability primitives: one event stream, one metrics file, one request id.
//!
//! Every request gets an id that travels frontend -> runtime -> worker -> event
//! stream -> `metrics.jsonl`, so cross-process debugging never falls back to
//! aligning timestamps by eye.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::{broadcast, watch};

/// Short, non-sequential id. Used for requests and spool entries alike.
pub fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Everything a frontend can learn without polling.
#[derive(Clone, Debug, Serialize)]
// `rename_all` renames variants; variant *fields* need `rename_all_fields`, and
// getting that wrong ships a snake_case wire contract by accident.
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    RuntimeReady {
        version: String,
    },
    RuntimeStopping,
    WorkerStarting {
        reason: String,
    },
    WorkerReady {
        port: Option<u16>,
        model_loaded: bool,
    },
    WorkerStopped {
        reason: String,
    },
    SpeakStarted {
        request_id: String,
        voice_pack_id: Option<String>,
        chars: usize,
    },
    /// The only event a subtitle/playback frontend needs: text to show, audio to
    /// fetch, and who spoke. Carries no audio bytes — presenters
    /// `GET /api/audio/{id}`; and no speaker metadata beyond the id, which they
    /// resolve through `GET /api/voices`.
    Speech {
        request_id: String,
        audio_id: String,
        text: String,
        display_text: Option<String>,
        /// Caller-supplied alignment between `display_text` and `text`, when it sent
        /// one. A presenter renders these directly; the alternative is guessing a
        /// mapping between two languages that do not line up positionally.
        ruby_pairs: Option<Vec<crate::service::RubyPair>>,
        voice_pack_id: Option<String>,
        duration_ms: u64,
        sample_rate: u32,
        display_seconds: Option<f64>,
    },
    SpeakFailed {
        request_id: String,
        code: String,
        message: String,
    },
    /// Long operations report here instead of only into a log file.
    Progress {
        request_id: Option<String>,
        phase: String,
        message: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub seq: u64,
    pub ts_ms: u64,
    #[serde(flatten)]
    pub event: Event,
}

const RECENT_CAPACITY: usize = 64;

/// Fan-out event bus. A late subscriber (tray restarted mid-utterance) gets
/// the recent tail replayed so it can render current state immediately.
pub struct Bus {
    tx: broadcast::Sender<Envelope>,
    seq: AtomicU64,
    recent: Mutex<VecDeque<Envelope>>,
    /// Set once, on shutdown. An open event stream is a connection that never
    /// ends on its own, so without this a single subscribed frontend blocks the
    /// server's graceful shutdown forever.
    stopping: watch::Sender<bool>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            tx,
            seq: AtomicU64::new(0),
            recent: Mutex::new(VecDeque::with_capacity(RECENT_CAPACITY)),
            stopping: watch::channel(false).0,
        }
    }

    /// Stamps the envelope, appends it to the replay tail and broadcasts it,
    /// all under the tail lock. Holding it across the send is what buys two
    /// guarantees: `seq` order is the order subscribers see it in (two
    /// concurrent publishers could otherwise broadcast 5 before 4, on a stream
    /// whose only ordering key is `seq`), and a subscriber that arrives between
    /// two publishes receives each envelope exactly once — see
    /// [`Bus::subscribe_with_tail`].
    pub fn publish(&self, event: Event) {
        match self.recent.lock() {
            Ok(mut recent) => {
                let envelope = self.stamp(event);
                if recent.len() == RECENT_CAPACITY {
                    recent.pop_front();
                }
                recent.push_back(envelope.clone());
                // No subscribers is normal (headless agent runs); not an error.
                let _ = self.tx.send(envelope);
            }
            // A poisoned tail costs a late subscriber its replay. A live one
            // must still get the event.
            Err(_) => {
                let _ = self.tx.send(self.stamp(event));
            }
        }
    }

    fn stamp(&self, event: Event) -> Envelope {
        Envelope {
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            ts_ms: now_ms(),
            event,
        }
    }

    /// Joins the live stream and takes the replay tail as one step.
    ///
    /// Both orders are wrong on their own. Snapshot-then-subscribe drops every
    /// event published in the gap: it is already past the snapshot and has no
    /// receiver yet, so it reaches nobody. Subscribe-then-snapshot delivers
    /// those events twice. Taking the tail lock around both — the same lock
    /// [`Bus::publish`] holds while it broadcasts — leaves neither a gap nor an
    /// overlap between the replayed tail and the live stream.
    pub fn subscribe_with_tail(&self) -> (broadcast::Receiver<Envelope>, Vec<Envelope>) {
        match self.recent.lock() {
            Ok(recent) => (self.tx.subscribe(), recent.iter().cloned().collect()),
            Err(_) => (self.tx.subscribe(), Vec::new()),
        }
    }

    /// How many frontends are listening. Replaces v1's trick of TCP-probing the
    /// tray's port to guess whether somebody else would play the audio.
    pub fn presenters(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Tells every open event stream to end.
    pub fn close(&self) {
        let _ = self.stopping.send(true);
    }

    /// Resolves when [`Bus::close`] has been called, including if it happened
    /// before this future was created.
    pub fn stopping(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut rx = self.stopping.subscribe();
        async move {
            loop {
                if *rx.borrow_and_update() {
                    return;
                }
                if rx.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

const LATENCY_SAMPLES: usize = 256;

/// Counters plus an append-only `metrics.jsonl`. The file is written by a
/// dedicated thread so no request ever blocks on disk, and the format matches
/// the zero-dependency observability contract used across these projects.
pub struct Metrics {
    speak_total: AtomicU64,
    speak_failed: AtomicU64,
    cold_starts: AtomicU64,
    audio_bytes: AtomicU64,
    served_bytes: AtomicU64,
    latencies: Mutex<VecDeque<u64>>,
    writer: Option<std::sync::mpsc::Sender<String>>,
}

impl Metrics {
    pub fn new(file: PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("vc-metrics".into())
            .spawn(move || {
                use std::io::Write;
                while let Ok(line) = rx.recv() {
                    let opened = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&file);
                    if let Ok(mut f) = opened {
                        let _ = f.write_all(line.as_bytes());
                        let _ = f.write_all(b"\n");
                    }
                }
            })
            .ok();
        Self {
            speak_total: AtomicU64::new(0),
            speak_failed: AtomicU64::new(0),
            cold_starts: AtomicU64::new(0),
            audio_bytes: AtomicU64::new(0),
            served_bytes: AtomicU64::new(0),
            latencies: Mutex::new(VecDeque::with_capacity(LATENCY_SAMPLES)),
            writer: Some(tx),
        }
    }

    pub fn record(&self, value: serde_json::Value) {
        if let (Some(tx), Ok(line)) = (&self.writer, serde_json::to_string(&value)) {
            let _ = tx.send(line);
        }
    }

    pub fn speak_ok(&self, total_ms: u64, audio_bytes: u64, cold: bool) {
        self.speak_total.fetch_add(1, Ordering::Relaxed);
        self.audio_bytes.fetch_add(audio_bytes, Ordering::Relaxed);
        if cold {
            self.cold_starts.fetch_add(1, Ordering::Relaxed);
        }
        if let Ok(mut samples) = self.latencies.lock() {
            if samples.len() == LATENCY_SAMPLES {
                samples.pop_front();
            }
            samples.push_back(total_ms);
        }
    }

    pub fn speak_err(&self) {
        self.speak_total.fetch_add(1, Ordering::Relaxed);
        self.speak_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn served(&self, bytes: u64) {
        self.served_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let mut samples: Vec<u64> = self
            .latencies
            .lock()
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        samples.sort_unstable();
        MetricsSnapshot {
            speak_total: self.speak_total.load(Ordering::Relaxed),
            speak_failed: self.speak_failed.load(Ordering::Relaxed),
            cold_starts: self.cold_starts.load(Ordering::Relaxed),
            audio_bytes: self.audio_bytes.load(Ordering::Relaxed),
            served_bytes: self.served_bytes.load(Ordering::Relaxed),
            speak_samples: samples.len(),
            speak_p50_ms: percentile(&samples, 50.0),
            speak_p95_ms: percentile(&samples, 95.0),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    pub speak_total: u64,
    pub speak_failed: u64,
    pub cold_starts: u64,
    pub audio_bytes: u64,
    pub served_bytes: u64,
    pub speak_samples: usize,
    pub speak_p50_ms: Option<u64>,
    pub speak_p95_ms: Option<u64>,
}

/// Nearest-rank percentile: `index = ceil(p/100 * n) - 1`. Interpolating
/// instead would report the maximum as the median on a two-sample set, which
/// misleads exactly when a fresh runtime has the fewest samples.
fn percentile(sorted: &[u64], pct: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((pct / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[7], 50.0), Some(7));
        assert_eq!(percentile(&[7], 95.0), Some(7));
        // Two samples: the median must be the lower one, not the maximum.
        assert_eq!(percentile(&[1627, 63284], 50.0), Some(1627));
        assert_eq!(percentile(&[1627, 63284], 95.0), Some(63284));
        let ten: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&ten, 50.0), Some(5));
        assert_eq!(percentile(&ten, 95.0), Some(10));
        assert_eq!(percentile(&ten, 100.0), Some(10));
    }
}
