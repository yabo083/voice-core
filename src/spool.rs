//! Audio spool: the mechanism that keeps audio out of RAM and out of JSON.
//!
//! The worker writes its WAV straight into a path this module reserves, so the
//! runtime never holds a sample in memory — it only stats the file and later
//! streams it from disk. Resident memory is therefore independent of utterance
//! length, and no base64 exists anywhere in the system.
//!
//! Ids are random and only resolvable through the in-memory index, so
//! `GET /api/audio/{id}` cannot be enumerated or coaxed into path traversal.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::obs::short_id;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub bytes: u64,
    pub sample_rate: u32,
    pub duration_ms: u64,
    created: Instant,
}

/// A reservation the runtime gave up on before its producer was finished with
/// the path. See [`Spool::abandon`].
struct Orphan {
    path: PathBuf,
    since: Instant,
}

pub struct Spool {
    dir: PathBuf,
    ttl: Duration,
    max_bytes: u64,
    index: Mutex<HashMap<String, Entry>>,
    orphans: Mutex<Vec<Orphan>>,
    total_bytes: AtomicU64,
}

impl Spool {
    /// Opens the spool directory and drops anything left behind by a previous
    /// run: spool contents are process-lifetime only, so a crash cannot leak
    /// disk forever.
    pub fn open(dir: PathBuf, ttl: Duration, max_bytes: u64) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("wav") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(Self {
            dir,
            ttl,
            max_bytes,
            index: Mutex::new(HashMap::new()),
            orphans: Mutex::new(Vec::new()),
            total_bytes: AtomicU64::new(0),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Hands out an id and the absolute path the producer must write to.
    /// Nothing is registered until [`Spool::commit`], so a failed synthesis
    /// leaves no index entry.
    pub fn reserve(&self) -> (String, PathBuf) {
        let id = short_id();
        let path = self.dir.join(format!("{id}.wav"));
        (id, path)
    }

    /// Registers a written file. Returns its size in bytes.
    pub fn commit(&self, id: &str, sample_rate: u32, duration_ms: u64) -> io::Result<u64> {
        let path = self.dir.join(format!("{id}.wav"));
        let bytes = std::fs::metadata(&path)?.len();
        let entry = Entry {
            path,
            bytes,
            sample_rate,
            duration_ms,
            created: Instant::now(),
        };
        if let Ok(mut index) = self.index.lock() {
            index.insert(id.to_string(), entry);
        }
        self.total_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.enforce_cap();
        Ok(bytes)
    }

    /// Discards a reservation whose producer failed.
    ///
    /// Invariant: from this call on the spool owns that path, not the request
    /// that gave up on it. That distinction is the whole point — a synthesis
    /// that missed its deadline is still running inside the worker and will
    /// write this exact file seconds or minutes later (`/synthesize` has no
    /// abort), so deleting a file that is not there yet deletes nothing and
    /// leaves a WAV that only the next [`Spool::open`] would reap. What cannot
    /// be deleted now is remembered and deleted by [`Spool::sweep`] once it
    /// appears. Either way it is never indexed, so it can never be served.
    pub fn abandon(&self, id: &str) {
        let path = self.dir.join(format!("{id}.wav"));
        // Also covers the Windows case where the worker still holds the handle:
        // the removal fails, so the path stays ours to retry.
        if std::fs::remove_file(&path).is_ok() {
            return;
        }
        if let Ok(mut orphans) = self.orphans.lock() {
            orphans.push(Orphan {
                path,
                since: Instant::now(),
            });
        }
    }

    pub fn get(&self, id: &str) -> Option<Entry> {
        self.index.lock().ok()?.get(id).cloned()
    }

    pub fn stats(&self) -> SpoolStats {
        let count = self.index.lock().map(|i| i.len()).unwrap_or(0);
        SpoolStats {
            entries: count,
            bytes: self.total_bytes.load(Ordering::Relaxed),
            max_bytes: self.max_bytes,
        }
    }

    /// Drops entries older than the TTL and finishes off abandoned
    /// reservations. Called on a timer by the service.
    pub fn sweep(&self) {
        let now = Instant::now();
        let expired: Vec<String> = match self.index.lock() {
            Ok(index) => index
                .iter()
                .filter(|(_, e)| now.duration_since(e.created) > self.ttl)
                .map(|(id, _)| id.clone())
                .collect(),
            Err(_) => return,
        };
        for id in expired {
            self.remove(&id);
        }
        self.reap_orphans(now);
    }

    /// Deletes each abandoned reservation as soon as its producer is done with
    /// it, and keeps the ones that are not there yet. `ttl` bounds the watch
    /// because it already bounds how long anything in this directory may live,
    /// and it is the longer of the two clocks that matter (3600 s against a
    /// 600 s synthesis deadline by default), so a worker that overran its
    /// deadline is still covered. Anything that outlives even that is caught by
    /// [`Spool::open`] on the next start.
    fn reap_orphans(&self, now: Instant) {
        let Ok(mut orphans) = self.orphans.lock() else {
            return;
        };
        orphans.retain(|orphan| {
            std::fs::remove_file(&orphan.path).is_err()
                && now.duration_since(orphan.since) <= self.ttl
        });
    }

    /// Evicts oldest-first until the byte cap is satisfied.
    fn enforce_cap(&self) {
        while self.total_bytes.load(Ordering::Relaxed) > self.max_bytes {
            let oldest = match self.index.lock() {
                Ok(index) => index
                    .iter()
                    .min_by_key(|(_, e)| e.created)
                    .map(|(id, _)| id.clone()),
                Err(_) => return,
            };
            match oldest {
                Some(id) => self.remove(&id),
                None => return,
            }
        }
    }

    fn remove(&self, id: &str) {
        let entry = match self.index.lock() {
            Ok(mut index) => index.remove(id),
            Err(_) => return,
        };
        if let Some(entry) = entry {
            self.total_bytes.fetch_sub(entry.bytes, Ordering::Relaxed);
            let _ = std::fs::remove_file(&entry.path);
        }
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpoolStats {
    pub entries: usize,
    pub bytes: u64,
    pub max_bytes: u64,
}
