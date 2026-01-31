use std::{convert::Infallible, path::PathBuf, pin::Pin, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get_service, post},
};
use futures::stream::{self, Stream, StreamExt};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
    service::TowerToHyperService,
};
use serde::Serialize;
use serde_json::to_string;
use socket2::{SockRef, TcpKeepalive};
use tokio::net::TcpListener;
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
};

mod metrics;
mod qemu;
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
    "-qmp",
    qemu::QMP_ARG,
    "-kernel",
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_crypto_provider();
    let kernel_path = qemu::resolve_kernel_path()?;
    let qemu = qemu::QemuManager::spawn(QEMU_ARGS, kernel_path).await?;

    let static_dir = resolve_static_dir()?;
    let index_html = static_dir.join("index.html");

    let state = AppState::new(qemu);
    let api = Router::new()
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
        .route("/api/qemu/reset", post(qemu_reset_handler))
        .route("/api/qemu/pause", post(qemu_pause_handler))
        .route("/api/qemu/resume", post(qemu_resume_handler))
        .layer(middleware::from_fn(api_no_store_middleware));

    let static_service = get_service(
        ServeDir::new(&static_dir)
            .precompressed_br()
            .precompressed_gzip()
            .append_index_html_on_directories(true)
            .fallback(ServeFile::new(&index_html)),
    )
    .layer(middleware::from_fn(static_cache_middleware));

    let app = Router::new()
        .merge(api)
        .fallback_service(static_service)
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn(hsts_middleware))
        .with_state(state);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    println!("http://localhost:{port}");
    let listener = match TcpListener::bind(format!("[::]:{port}")).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind(format!("0.0.0.0:{port}")).await?,
    };

    let mut conn_builder = auto::Builder::new(TokioExecutor::new());
    conn_builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(None)
        .keep_alive(true)
        .half_close(true);

    loop {
        let (stream, _peer) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let _ = set_tcp_keepalive(&stream);

        let app = app.clone();
        let conn_builder = conn_builder.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = TowerToHyperService::new(app);
            if let Err(err) = conn_builder.serve_connection(io, svc).await {
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

#[derive(Serialize)]
struct ControlResponse {
    status: &'static str,
}

async fn qemu_reset_handler(
    State(state): State<AppState>,
) -> Result<Json<ControlResponse>, (StatusCode, String)> {
    state
        .qemu()
        .reset()
        .await
        .map(|_| Json(ControlResponse { status: "ok" }))
        .map_err(internal_error)
}

async fn qemu_pause_handler(
    State(state): State<AppState>,
) -> Result<Json<ControlResponse>, (StatusCode, String)> {
    state
        .qemu()
        .pause()
        .await
        .map(|_| Json(ControlResponse { status: "ok" }))
        .map_err(internal_error)
}

async fn qemu_resume_handler(
    State(state): State<AppState>,
) -> Result<Json<ControlResponse>, (StatusCode, String)> {
    state
        .qemu()
        .resume()
        .await
        .map(|_| Json(ControlResponse { status: "ok" }))
        .map_err(internal_error)
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn set_tcp_keepalive(stream: &tokio::net::TcpStream) -> std::io::Result<()> {
    let sock_ref = SockRef::from(stream);
    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(120))
        .with_interval(Duration::from_secs(30));
    sock_ref.set_tcp_keepalive(&keepalive)
}

async fn api_no_store_middleware(req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

async fn static_cache_middleware(req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    if res.headers().contains_key(header::CACHE_CONTROL) {
        return res;
    }
    let cache_value = if is_html_response(&res) {
        "no-cache, max-age=0, must-revalidate"
    } else {
        "public, max-age=31536000, immutable"
    };
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache_value));
    res
}

async fn hsts_middleware(req: Request<Body>, next: Next) -> Response {
    let is_https = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("https"))
        .unwrap_or(false);
    let mut res = next.run(req).await;
    if is_https {
        res.headers_mut()
            .entry(header::STRICT_TRANSPORT_SECURITY)
            .or_insert(HeaderValue::from_static("max-age=31536000"));
    }
    res
}

fn is_html_response(res: &Response) -> bool {
    res.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"))
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
