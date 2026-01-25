use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    routing::{get_service, post},
};
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
    process::Command,
};
use tower_http::services::{ServeDir, ServeFile};

mod webrtc_gateway;

use webrtc_gateway::{
    AppState, VNC_PORT_BASE, VNC_PORT_COUNT, candidate_handler, config_handler, offer_handler,
};

const DEFAULT_PORT: u16 = 8080;
const DEBUG_LOG_PATH: &str = "/Users/andrewhinh/Desktop/projects/web-os/.cursor/debug.log";

fn debug_log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    // #region agent log
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(DEBUG_LOG_PATH)
    {
        let entry = serde_json::json!({
            "sessionId": "debug-session",
            "runId": "run1",
            "hypothesisId": hypothesis_id,
            "location": location,
            "message": message,
            "data": data,
            "timestamp": ts,
        });
        let _ = writeln!(file, "{}", entry);
    }
    // #endregion
}
fn build_netdev_arg() -> String {
    let mut arg = "user,id=net0".to_string();
    for port in VNC_PORT_BASE..(VNC_PORT_BASE + VNC_PORT_COUNT) {
        arg.push_str(&format!(",hostfwd=tcp:127.0.0.1:{port}-:{port}"));
    }
    arg
}

fn qemu_args() -> Vec<String> {
    vec![
        "-machine".into(),
        "virt,aia=aplic-imsic".into(),
        "-bios".into(),
        "none".into(),
        "-m".into(),
        "512M".into(),
        "-smp".into(),
        "4".into(),
        "-serial".into(),
        "mon:stdio".into(),
        "-global".into(),
        "virtio-mmio.force-legacy=false".into(),
        "-drive".into(),
        "file=target/fs.img,if=none,format=raw,id=x0".into(),
        "-device".into(),
        "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0".into(),
        "-netdev".into(),
        build_netdev_arg(),
        "-device".into(),
        "virtio-net-device,netdev=net0,bus=virtio-mmio-bus.1".into(),
        "-device".into(),
        "virtio-gpu-device,bus=virtio-mmio-bus.2,hostmem=256M".into(),
        "-device".into(),
        "virtio-keyboard-device,bus=virtio-mmio-bus.3".into(),
        "-device".into(),
        "virtio-mouse-device,bus=virtio-mmio-bus.4".into(),
        "-vnc".into(),
        "127.0.0.1:0,lossy=on,non-adaptive=on,key-delay-ms=0".into(),
        "-kernel".into(),
    ]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    spawn_qemu().await?;

    let static_dir = resolve_static_dir()?;
    let index_html = static_dir.join("index.html");

    let state = AppState::default();
    let app = Router::new()
        .route("/api/webrtc/config", axum::routing::get(config_handler))
        .route("/api/webrtc/offer", post(offer_handler))
        .route("/api/webrtc/candidate", post(candidate_handler))
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
    cmd.args(qemu_args());
    cmd.arg(&kernel_path);

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug_log(
                    "Q",
                    "crates/app/src/main.rs:qemu_stdout",
                    "qemu_line",
                    serde_json::json!({ "line": line }),
                );
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug_log(
                    "Q",
                    "crates/app/src/main.rs:qemu_stderr",
                    "qemu_line",
                    serde_json::json!({ "line": line }),
                );
            }
        });
    }

    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => eprintln!("QEMU exited: {status}"),
            Err(err) => eprintln!("QEMU wait failed: {err}"),
        }
    });

    Ok(())
}
