//! voice-core v2 — a local TTS runtime with one public surface.
//!
//! Layering, top to bottom, with dependencies pointing only downward:
//!
//! | layer      | lives in                     | must not know                      |
//! |------------|------------------------------|------------------------------------|
//! | presenter  | `voice-core` CLI, tray, agent | worker ports, model paths, venvs   |
//! | runtime    | this crate                    | how output is presented            |
//! | worker     | `worker/irodori/worker.py`    | who called, subtitles, pack registry |
//!
//! The runtime never calls a frontend back. Frontends subscribe to
//! `GET /api/events`; that is the whole presentation contract.

use std::io;
use std::sync::Arc;

use tokio::sync::oneshot;

pub mod api;
pub mod config;
pub mod engine;
pub mod error;
pub mod jsonc;
pub mod obs;
pub mod packs;
pub mod service;
pub mod spool;
pub mod supervise;

pub use config::{Config, WorkerSource, WorkerSpec};
pub use service::{Service, API_VERSION, RUNTIME_VERSION};

pub struct Assembled {
    pub service: Arc<Service>,
    pub shutdown: oneshot::Receiver<()>,
}

/// Wires the runtime together. Nothing here starts a worker or touches the GPU:
/// startup is cheap and the first `speak` pays for what it needs.
pub fn assemble(cfg: Config) -> io::Result<Assembled> {
    cfg.prepare_dirs()?;
    let cfg = Arc::new(cfg);

    let bus = Arc::new(obs::Bus::new());
    let metrics = Arc::new(obs::Metrics::new(cfg.metrics_file()));
    let spool = Arc::new(spool::Spool::open(
        cfg.spool_dir(),
        cfg.spool_ttl,
        cfg.spool_max_bytes,
    )?);
    let worker = Arc::new(supervise::Worker::new(
        cfg.worker.clone(),
        cfg.log_dir(),
        Arc::clone(&bus),
        cfg.worker_ready_timeout,
    ));

    let (tx, rx) = oneshot::channel();
    let service = Arc::new(Service::new(
        Arc::clone(&cfg),
        spool,
        Arc::clone(&bus),
        metrics,
        worker,
        tx,
    ));
    service.spawn_background();
    bus.publish(obs::Event::RuntimeReady {
        version: RUNTIME_VERSION.to_string(),
    });

    Ok(Assembled {
        service,
        shutdown: rx,
    })
}

pub async fn bind(addr: std::net::SocketAddr) -> io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr).await
}

/// Serves until `/api/shutdown` fires or the process is interrupted. Both paths
/// really return, and returning drops the worker's job object, which is what
/// terminates the engine process tree.
pub async fn serve(
    listener: tokio::net::TcpListener,
    service: Arc<Service>,
    shutdown: oneshot::Receiver<()>,
) -> io::Result<()> {
    let app = api::router(Arc::clone(&service));
    let (relay, relayed) = oneshot::channel::<()>();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = relayed.await;
            })
            .await
    });

    // Either the server ends on its own, or something asks us to stop.
    let self_ended = tokio::select! {
        joined = &mut server => Some(joined),
        _ = shutdown => None,
        _ = tokio::signal::ctrl_c() => None,
    };

    let result = match self_ended {
        Some(joined) => joined.map_err(io::Error::other)?,
        None => {
            // Close the event streams first, then let in-flight requests drain
            // under a hard bound so no connection can keep the process alive.
            service.bus().close();
            let _ = relay.send(());
            match tokio::time::timeout(std::time::Duration::from_secs(5), server).await {
                Ok(joined) => joined.map_err(io::Error::other)?,
                Err(_) => {
                    eprintln!("graceful shutdown exceeded 5s; exiting with connections open");
                    Ok(())
                }
            }
        }
    };

    service.sleep().await;
    result
}
