use std::{convert::Infallible, path::PathBuf, pin::Pin, process::Stdio, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get_service, post},
};
use futures::stream::{self, Stream, StreamExt};
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use serde_json::to_string;
use tokio::{net::TcpListener, process::Command};
use tower_http::services::{ServeDir, ServeFile};

mod metrics;
mod webrtc_gateway;

use metrics::MetricsSnapshot;
use webrtc_gateway::{AppState, candidate_handler, config_handler, offer_handler, stream_handler};

const DEFAULT_PORT: u16 = 8080;
const QEMU_ARGS: &[&str] = &[
    "-machine",
    "virt,aia=aplic-imsic",
    "-bios",
    "none",
    "-m",
    "512M",
    "-smp",
    "4",
    "-serial",
    "mon:stdio",
    "-global",
    "virtio-mmio.force-legacy=false",
    "-drive",
    "file=target/fs.img,if=none,format=raw,id=x0",
    "-device",
    "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
    "-netdev",
    "user,id=net0",
    "-device",
    "virtio-net-device,netdev=net0,bus=virtio-mmio-bus.1",
    "-device",
    "virtio-gpu-device,bus=virtio-mmio-bus.2,hostmem=256M",
    "-device",
    "virtio-keyboard-device,bus=virtio-mmio-bus.3",
    "-device",
    "virtio-mouse-device,bus=virtio-mmio-bus.4",
    "-vnc",
    "127.0.0.1:0,lossy=on,non-adaptive=on,key-delay-ms=0",
    "-kernel",
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_crypto_provider();
    spawn_qemu().await?;

    let static_dir = resolve_static_dir()?;
    let index_html = static_dir.join("index.html");

    let state = AppState::default();
    let app = Router::new()
        .route("/api/webrtc/config", axum::routing::get(config_handler))
        .route("/api/webrtc/offer", post(offer_handler))
        .route("/api/webrtc/candidate", post(candidate_handler))
        .route("/api/webrtc/stream", axum::routing::get(stream_handler))
        .route("/api/metrics", axum::routing::get(metrics_get_handler))
        .route(
            "/api/metrics/stream",
            axum::routing::get(metrics_stream_handler),
        )
        .route("/api/metrics/visit", post(metrics_visit_handler))
        .route("/api/metrics/run-cmd", post(metrics_run_cmd_handler))
        .fallback_service(get_service(
            ServeDir::new(&static_dir)
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new(&index_html)),
        ));
    let app = app.with_state(state);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    println!("http://localhost:{port}");
    let listener = match TcpListener::bind(format!("[::]:{port}")).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind(format!("0.0.0.0:{port}")).await?,
    };

    loop {
        let (stream, _peer) = listener.accept().await?;
        let _ = stream.set_nodelay(true);

        let app = app.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = TowerToHyperService::new(app);
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, svc)
                .with_upgrades()
                .await
            {
                eprintln!("HTTP conn error: {err}");
            }
        });
    }
}

fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

async fn metrics_get_handler(
    State(state): State<AppState>,
) -> Result<Json<MetricsSnapshot>, (StatusCode, String)> {
    state
        .metrics()
        .snapshot()
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn metrics_visit_handler(
    State(state): State<AppState>,
) -> Result<Json<MetricsSnapshot>, (StatusCode, String)> {
    let snapshot = state
        .metrics()
        .incr_visitors()
        .await
        .map_err(internal_error)?;
    state.publish_metrics(snapshot);
    Ok(Json(snapshot))
}

async fn metrics_run_cmd_handler(
    State(state): State<AppState>,
) -> Result<Json<MetricsSnapshot>, (StatusCode, String)> {
    let snapshot = state
        .metrics()
        .incr_run_cmds()
        .await
        .map_err(internal_error)?;
    state.publish_metrics(snapshot);
    Ok(Json(snapshot))
}

async fn metrics_stream_handler(State(state): State<AppState>) -> Response {
    let snapshot = match state.metrics().snapshot().await {
        Ok(snapshot) => snapshot,
        Err(err) => return internal_error(err).into_response(),
    };
    let initial = Event::default().data(to_string(&snapshot).unwrap_or_else(|_| "{}".to_string()));
    let rx = state.metrics_subscribe();

    let updates = stream::unfold(rx, |mut rx| async {
        loop {
            match rx.recv().await {
                Ok(snapshot) => {
                    let data = to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
                    let event = Event::default().data(data);
                    return Some((Ok(event), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    let stream = stream::once(async move { Ok::<Event, Infallible>(initial) }).chain(updates);
    let stream: Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> = Box::pin(stream);
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn resolve_static_dir() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let fallback = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../build"));

    let resolved = [cwd.join("build"), cwd.join("../build"), fallback.clone()]
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or(fallback);

    Ok(resolved)
}

async fn spawn_qemu() -> anyhow::Result<()> {
    let kernel_path = ["release", "debug"]
        .into_iter()
        .map(|p| PathBuf::from(format!("target/riscv64gc-unknown-none-elf/{p}/web-os")))
        .find(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("Kernel not found. Run `cargo build` first."))?;

    let mut cmd = Command::new("qemu-system-riscv64");
    cmd.args(QEMU_ARGS);
    cmd.arg(&kernel_path);

    cmd.stdin(Stdio::null());

    let mut child = cmd.spawn()?;

    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => eprintln!("QEMU exited: {status}"),
            Err(err) => eprintln!("QEMU wait failed: {err}"),
        }
    });

    Ok(())
}
