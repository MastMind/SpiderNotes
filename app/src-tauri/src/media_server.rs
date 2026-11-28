//! Local HTTP server for vault media. WebKit's media pipeline cannot play
//! large videos from the custom `asset:` scheme (it probes with a Range
//! request and fails) nor from a large `blob:` URL (stalls in
//! NETWORK_LOADING — verified on WebKitGTK 2.52.6). Plain HTTP works: the
//! element buffers the whole file. So `<video>`/`<audio>` resolve to
//! `http://127.0.0.1:<port>/<rel>` while this server serves the vault's
//! hidden `.assets/` dir. Loopback-only, ephemeral port, no auth.

use std::path::Path;

use anyhow::{Context, Result};
use axum::Router;
use tower_http::services::ServeDir;

pub struct MediaServer {
    pub port: u16,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MediaServer {
    /// Stop the accept loop (a no-op once the task already finished).
    pub fn stop(self) {
        if let Some(tx) = self.stop_tx {
            let _ = tx.send(());
        }
    }
}

/// Bind 127.0.0.1:<ephemeral> and serve `assets_dir` (files only; no
/// directory listing). The directory is created when missing so media
/// imported later is playable without an app restart.
pub async fn start(assets_dir: &Path) -> Result<MediaServer> {
    if let Err(e) = tokio::fs::create_dir_all(assets_dir).await {
        tracing::warn!("media server: cannot create {}: {e}", assets_dir.display());
    }
    let app = Router::new().fallback_service(ServeDir::new(assets_dir));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .with_context(|| {
            format!(
                "bind 127.0.0.1 for the media server of {}",
                assets_dir.display()
            )
        })?;
    let port = listener
        .local_addr()
        .map_err(|e| anyhow::anyhow!("media server local addr: {e}"))?
        .port();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let dir = assets_dir.to_path_buf();
    tokio::spawn(async move {
        tracing::info!(
            "media server: serving {} on 127.0.0.1:{port}",
            dir.display()
        );
        tokio::select! {
            res = axum::serve(listener, app) => {
                if let Err(e) = res {
                    tracing::warn!("media server on {port} exited: {e}");
                }
            }
            _ = stop_rx => {
                tracing::info!("media server on {port} stopped");
            }
        }
    });
    Ok(MediaServer {
        port,
        stop_tx: Some(stop_tx),
    })
}
