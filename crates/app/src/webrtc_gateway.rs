use std::{
    collections::{HashMap, VecDeque},
    fs::OpenOptions,
    io::Write,
    net::IpAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use ice::udp_network::{EphemeralUDP, UDPNetwork};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex, Notify, mpsc},
};
use uuid::Uuid;
use webrtc::{
    api::{APIBuilder, setting_engine::SettingEngine},
    data_channel::RTCDataChannel,
    ice_transport::{
        ice_candidate::RTCIceCandidateInit, ice_candidate_type::RTCIceCandidateType,
        ice_server::RTCIceServer,
    },
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
    },
};

const DEFAULT_STUN_SERVER: &str = "stun:stun.l.google.com:19302";
const DEFAULT_SESSION_OWNER: &str = "local";
pub const VNC_PORT_BASE: u16 = 5901;
pub const VNC_PORT_COUNT: u16 = 8;
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

#[derive(Clone)]
pub struct AppState {
    sessions: Arc<Mutex<HashMap<Uuid, Session>>>,
    ports: Arc<Mutex<VecDeque<u16>>>,
}

struct Session {
    pc: Arc<RTCPeerConnection>,
    pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>>,
}

impl Default for AppState {
    fn default() -> Self {
        let ports = (0..VNC_PORT_COUNT)
            .map(|i| VNC_PORT_BASE + i)
            .collect::<VecDeque<_>>();
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            ports: Arc::new(Mutex::new(ports)),
        }
    }
}

#[derive(Deserialize)]
pub struct OfferRequest {
    pub sdp: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Serialize)]
pub struct OfferResponse {
    pub session_id: String,
    pub sdp: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize)]
pub struct CandidateRequest {
    pub session_id: String,
    pub candidate: Option<TrickleCandidate>,
}

#[derive(Serialize)]
pub struct CandidateResponse {
    pub candidates: Vec<TrickleCandidate>,
}

#[derive(Serialize)]
pub struct WebrtcConfigResponse {
    pub ice_servers: Vec<WebrtcIceServer>,
}

#[derive(Serialize)]
pub struct WebrtcIceServer {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrickleCandidate {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    pub sdp_mline_index: Option<u16>,
    pub username_fragment: Option<String>,
}

pub async fn offer_handler(
    State(state): State<AppState>,
    Json(req): Json<OfferRequest>,
) -> Result<Response, (StatusCode, String)> {
    if req.kind.to_lowercase() != "offer" {
        return Err((StatusCode::BAD_REQUEST, "expected type=offer".into()));
    }

    let session_uuid = Uuid::new_v4();
    let pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>> = Arc::new(Mutex::new(Vec::new()));
    let session_owner = current_session_owner();

    let vnc_port = allocate_vnc_port(state.ports.clone())
        .await
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no seats available".into()))?;
    debug_log(
        "A",
        "crates/app/src/webrtc_gateway.rs:offer_handler",
        "offer_alloc",
        serde_json::json!({
            "session_uuid": session_uuid.to_string(),
            "vnc_port": vnc_port,
        }),
    );

    let pc = match create_peer_connection(pending_candidates.clone(), vnc_port).await {
        Ok(pc) => pc,
        Err(err) => {
            release_vnc_port(state.ports.clone(), vnc_port).await;
            return Err(internal_error(err));
        }
    };

    if let Err(err) = pc
        .set_remote_description(RTCSessionDescription::offer(req.sdp).map_err(internal_error)?)
        .await
    {
        release_vnc_port(state.ports.clone(), vnc_port).await;
        return Err(internal_error(err));
    }

    let answer = match pc.create_answer(None).await {
        Ok(answer) => answer,
        Err(err) => {
            release_vnc_port(state.ports.clone(), vnc_port).await;
            return Err(internal_error(err));
        }
    };
    if let Err(err) = pc.set_local_description(answer.clone()).await {
        release_vnc_port(state.ports.clone(), vnc_port).await;
        return Err(internal_error(err));
    }

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            session_uuid,
            Session {
                pc: pc.clone(),
                pending_candidates,
            },
        );
    }
    register_session_cleanup(
        session_uuid,
        state.sessions.clone(),
        state.ports.clone(),
        pc.clone(),
        vnc_port,
    );

    Ok(Json(OfferResponse {
        session_id: encode_session_id(&session_owner, session_uuid),
        sdp: answer.sdp,
        kind: "answer".into(),
    })
    .into_response())
}

pub async fn candidate_handler(
    State(state): State<AppState>,
    Json(req): Json<CandidateRequest>,
) -> Result<Response, (StatusCode, String)> {
    let inbound = req.candidate.is_some();
    let current_owner = current_session_owner();
    let (owner, session_uuid) = decode_session_id(&req.session_id, &current_owner)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "invalid session_id".into()))?;

    if owner != current_owner {
        return Ok(fly_replay_response(&owner));
    }

    let (pc, pending_candidates) = {
        let sessions = state.sessions.lock().await;
        let Some(sess) = sessions.get(&session_uuid) else {
            return Err((StatusCode::NOT_FOUND, "unknown session".into()));
        };
        (sess.pc.clone(), sess.pending_candidates.clone())
    };

    if let Some(c) = req.candidate {
        pc.add_ice_candidate(from_trickle(c))
            .await
            .map_err(internal_error)?;
    }

    let candidates = drain_pending_candidates(pending_candidates).await;
    debug_log(
        "C",
        "crates/app/src/webrtc_gateway.rs:candidate_handler",
        "candidate_sync",
        serde_json::json!({
            "session_uuid": session_uuid.to_string(),
            "inbound": inbound,
            "outbound_count": candidates.len(),
        }),
    );

    Ok(Json(CandidateResponse { candidates }).into_response())
}

pub async fn config_handler() -> impl IntoResponse {
    let ice_servers = build_ice_servers()
        .into_iter()
        .map(|s| WebrtcIceServer {
            urls: s.urls,
            username: empty_to_none(s.username),
            credential: empty_to_none(s.credential),
        })
        .collect();
    let mut res = Json(WebrtcConfigResponse { ice_servers }).into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    res
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn current_session_owner() -> String {
    std::env::var("FLY_ALLOC_ID").unwrap_or_else(|_| DEFAULT_SESSION_OWNER.to_string())
}

fn encode_session_id(owner: &str, id: Uuid) -> String {
    let sanitized_owner = if owner.is_empty() {
        DEFAULT_SESSION_OWNER
    } else {
        owner
    };
    format!("{sanitized_owner}:{id}")
}

fn decode_session_id(raw: &str, default_owner: &str) -> Option<(String, Uuid)> {
    let (owner, id_str) = match raw.split_once(':') {
        Some((owner, id_str)) if !owner.is_empty() && !id_str.is_empty() => (owner, id_str),
        _ => (default_owner, raw),
    };
    let id = Uuid::parse_str(id_str).ok()?;
    Some((owner.to_string(), id))
}

fn fly_replay_response(instance: &str) -> Response {
    let mut res = (StatusCode::CONFLICT, "replay").into_response();
    if let Ok(value) = HeaderValue::from_str(&format!("instance={instance}")) {
        res.headers_mut()
            .insert(header::HeaderName::from_static("fly-replay"), value);
    }
    res
}

async fn create_peer_connection(
    pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>>,
    vnc_port: u16,
) -> anyhow::Result<Arc<RTCPeerConnection>> {
    let setting_engine = build_setting_engine().await?;
    let api = APIBuilder::new()
        .with_setting_engine(setting_engine)
        .build();

    let pc = Arc::new(
        api.new_peer_connection(RTCConfiguration {
            ice_servers: build_ice_servers(),
            ..Default::default()
        })
        .await?,
    );

    register_pending_candidate_handler(pc.clone(), pending_candidates);
    register_vnc_channel(pc.clone(), vnc_port);

    Ok(pc)
}

async fn bridge_vnc(
    dc: Arc<RTCDataChannel>,
    closed: Arc<Notify>,
    vnc_port: u16,
) -> anyhow::Result<()> {
    let connect_result = TcpStream::connect(format!("127.0.0.1:{vnc_port}")).await;
    debug_log(
        "B",
        "crates/app/src/webrtc_gateway.rs:bridge_vnc",
        "bridge_connect",
        serde_json::json!({
            "vnc_port": vnc_port,
            "ok": connect_result.is_ok(),
            "error": connect_result.as_ref().err().map(|e| e.to_string()),
        }),
    );
    let stream = connect_result?;
    let _ = stream.set_nodelay(true);
    let (tcp_r, tcp_w) = stream.into_split();

    let (tx, mut rx) = mpsc::channel::<Bytes>(1024);
    dc.on_message(Box::new(move |msg| {
        let tx = tx.clone();
        Box::pin(async move {
            if msg.is_string {
                return;
            }
            let _ = tx.send(msg.data).await;
        })
    }));

    let read_task = tokio::spawn(tcp_to_dc_loop(tcp_r, dc.clone(), closed.clone(), vnc_port));

    dc_to_tcp_loop(&mut rx, tcp_w, closed).await?;

    let _ = read_task.await;
    Ok(())
}

fn build_ice_servers() -> Vec<RTCIceServer> {
    let stun_servers = std::env::var("STUN_SERVERS").unwrap_or_else(|_| DEFAULT_STUN_SERVER.into());
    let stun_urls: Vec<String> = stun_servers
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    vec![RTCIceServer {
        urls: if stun_urls.is_empty() {
            vec![DEFAULT_STUN_SERVER.to_string()]
        } else {
            stun_urls
        },
        ..Default::default()
    }]
}

fn parse_port_range(s: &str) -> Option<(u16, u16)> {
    let (a, b) = s.split_once('-')?;
    let min = a.trim().parse::<u16>().ok()?;
    let max = b.trim().parse::<u16>().ok()?;
    if min == 0 || max == 0 || min > max {
        return None;
    }
    Some((min, max))
}

async fn resolve_fly_global_services_ip() -> Option<IpAddr> {
    let mut addrs = tokio::net::lookup_host(("fly-global-services", 0))
        .await
        .ok()?;
    addrs.next().map(|sa| sa.ip())
}

fn empty_to_none(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn from_trickle(c: TrickleCandidate) -> RTCIceCandidateInit {
    RTCIceCandidateInit {
        candidate: c.candidate,
        sdp_mid: c.sdp_mid,
        sdp_mline_index: c.sdp_mline_index,
        username_fragment: c.username_fragment,
    }
}

fn to_trickle(c: RTCIceCandidateInit) -> TrickleCandidate {
    TrickleCandidate {
        candidate: c.candidate,
        sdp_mid: c.sdp_mid,
        sdp_mline_index: c.sdp_mline_index,
        username_fragment: c.username_fragment,
    }
}

async fn drain_pending_candidates(
    pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>>,
) -> Vec<TrickleCandidate> {
    let mut q = pending_candidates.lock().await;
    q.drain(..).map(to_trickle).collect()
}

fn parse_public_ips() -> Vec<String> {
    let Ok(ips) = std::env::var("ICE_PUBLIC_IPS") else {
        return Vec::new();
    };
    ips.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn parse_port_range_env() -> Option<(u16, u16)> {
    let Ok(range) = std::env::var("ICE_PORT_RANGE") else {
        return None;
    };
    parse_port_range(&range)
}

async fn build_setting_engine() -> anyhow::Result<SettingEngine> {
    let mut setting_engine = SettingEngine::default();
    let is_fly = std::env::var("FLY_APP_NAME").is_ok();

    if !is_fly {
        setting_engine.set_include_loopback_candidate(true);
    }

    if is_fly {
        if let Some(fly_ip) = resolve_fly_global_services_ip().await {
            setting_engine.set_ip_filter(Box::new(move |ip: IpAddr| ip == fly_ip));
        }
    }

    let ips = parse_public_ips();
    if !ips.is_empty() {
        setting_engine.set_nat_1to1_ips(ips, RTCIceCandidateType::Host);
    }

    if let Some((min, max)) = parse_port_range_env() {
        setting_engine.set_udp_network(UDPNetwork::Ephemeral(EphemeralUDP::new(min, max)?));
    }

    Ok(setting_engine)
}

async fn allocate_vnc_port(ports: Arc<Mutex<VecDeque<u16>>>) -> Option<u16> {
    let mut ports = ports.lock().await;
    ports.pop_front()
}

async fn release_vnc_port(ports: Arc<Mutex<VecDeque<u16>>>, port: u16) {
    let mut ports = ports.lock().await;
    ports.push_back(port);
}

fn register_pending_candidate_handler(
    pc: Arc<RTCPeerConnection>,
    pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>>,
) {
    pc.on_ice_candidate(Box::new(move |cand| {
        let q = pending_candidates.clone();
        Box::pin(async move {
            if let Some(cand) = cand {
                if let Ok(json) = cand.to_json() {
                    q.lock().await.push(json);
                }
            }
        })
    }));
}

fn register_vnc_channel(pc: Arc<RTCPeerConnection>, vnc_port: u16) {
    pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
        let port = vnc_port;
        Box::pin(async move {
            if dc.label() != "vnc" {
                return;
            }

            let closed_notify = Arc::new(Notify::new());
            register_dc_close(&dc, closed_notify.clone());
            register_dc_open(&dc, closed_notify, port);
        })
    }));
}

fn register_dc_close(dc: &Arc<RTCDataChannel>, closed_notify: Arc<Notify>) {
    dc.on_close(Box::new(move || {
        let notify = closed_notify.clone();
        Box::pin(async move {
            notify.notify_waiters();
        })
    }));
}

fn register_dc_open(dc: &Arc<RTCDataChannel>, closed_notify: Arc<Notify>, vnc_port: u16) {
    let dc_for_open = dc.clone();
    let port = vnc_port;
    dc.on_open(Box::new(move || {
        let dc = dc_for_open.clone();
        let closed = closed_notify.clone();
        Box::pin(async move {
            debug_log(
                "D",
                "crates/app/src/webrtc_gateway.rs:register_dc_open",
                "dc_open",
                serde_json::json!({
                    "label": dc.label(),
                    "vnc_port": port,
                }),
            );
            tokio::spawn(async move {
                if let Err(err) = bridge_vnc(dc, closed, port).await {
                    eprintln!("VNC bridge error: {err}");
                }
            });
        })
    }));
}

fn register_session_cleanup(
    session_id: Uuid,
    sessions: Arc<Mutex<HashMap<Uuid, Session>>>,
    ports: Arc<Mutex<VecDeque<u16>>>,
    pc: Arc<RTCPeerConnection>,
    vnc_port: u16,
) {
    pc.on_peer_connection_state_change(Box::new(move |state| {
        let sessions = sessions.clone();
        let ports = ports.clone();
        Box::pin(async move {
            if matches!(
                state,
                RTCPeerConnectionState::Closed
                    | RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Disconnected
            ) {
                let mut sessions = sessions.lock().await;
                if sessions.remove(&session_id).is_some() {
                    let mut ports = ports.lock().await;
                    ports.push_back(vnc_port);
                }
            }
        })
    }));
}

async fn tcp_to_dc_loop(
    mut tcp_r: OwnedReadHalf,
    dc: Arc<RTCDataChannel>,
    closed: Arc<Notify>,
    vnc_port: u16,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 16 * 1024];
    debug_log(
        "H",
        "crates/app/src/webrtc_gateway.rs:tcp_to_dc_loop",
        "tcp_read_start",
        serde_json::json!({ "vnc_port": vnc_port }),
    );
    let mut logged = 0usize;
    let mut total_bytes: usize = 0;
    let marks: [usize; 4] = [256 * 1024, 1024 * 1024, 2 * 1024 * 1024, 3 * 1024 * 1024];
    let mut mark_idx = 0usize;
    let mut logged_frame = 0usize;
    loop {
        tokio::select! {
            _ = closed.notified() => break,
            res = tcp_r.read(&mut buf) => {
                let n = match res {
                    Ok(0) => {
                        debug_log(
                            "H",
                            "crates/app/src/webrtc_gateway.rs:tcp_to_dc_loop",
                            "tcp_read_eof",
                            serde_json::json!({ "vnc_port": vnc_port }),
                        );
                        break;
                    }
                    Ok(n) => n,
                    Err(err) => {
                        debug_log(
                            "H",
                            "crates/app/src/webrtc_gateway.rs:tcp_to_dc_loop",
                            "tcp_read_err",
                            serde_json::json!({ "vnc_port": vnc_port, "error": err.to_string() }),
                        );
                        return Err(err.into());
                    }
                };
                let first = buf.get(0).copied().unwrap_or(0);
                if logged < 10 {
                    debug_log(
                        "H",
                        "crates/app/src/webrtc_gateway.rs:tcp_to_dc_loop",
                        "tcp_read",
                        serde_json::json!({
                            "bytes": n,
                            "idx": logged + 1,
                            "first": first
                        }),
                    );
                    logged += 1;
                }
                let frame_log = (first == 0 || n > 1024) && logged_frame < 5;
                let frame_idx = if frame_log { logged_frame + 1 } else { 0 };
                if frame_log {
                    // #region agent log
                    debug_log(
                        "H",
                        "crates/app/src/webrtc_gateway.rs:tcp_to_dc_loop",
                        "tcp_read_frame",
                        serde_json::json!({ "bytes": n, "idx": frame_idx, "first": first }),
                    );
                    // #endregion
                }
                total_bytes = total_bytes.saturating_add(n);
                while mark_idx < marks.len() && total_bytes >= marks[mark_idx] {
                    // #region agent log
                    debug_log(
                        "H",
                        "crates/app/src/webrtc_gateway.rs:tcp_to_dc_loop",
                        "tcp_read_mark",
                        serde_json::json!({
                            "vnc_port": vnc_port,
                            "total": total_bytes,
                            "mark": marks[mark_idx]
                        }),
                    );
                    // #endregion
                    mark_idx += 1;
                }
                let bytes = Bytes::copy_from_slice(&buf[..n]);
                if let Err(err) = dc.send(&bytes).await {
                    debug_log(
                        "H",
                        "crates/app/src/webrtc_gateway.rs:tcp_to_dc_loop",
                        "dc_send_err",
                        serde_json::json!({ "error": err.to_string() }),
                    );
                    return Err(err.into());
                }
                if frame_log {
                    // #region agent log
                    debug_log(
                        "H",
                        "crates/app/src/webrtc_gateway.rs:tcp_to_dc_loop",
                        "dc_send_ok",
                        serde_json::json!({ "bytes": n, "idx": frame_idx }),
                    );
                    // #endregion
                    logged_frame += 1;
                }
            }
        }
    }
    Ok(())
}

async fn dc_to_tcp_loop(
    rx: &mut mpsc::Receiver<Bytes>,
    mut tcp_w: OwnedWriteHalf,
    closed: Arc<Notify>,
) -> anyhow::Result<()> {
    let mut logged = 0usize;
    loop {
        tokio::select! {
            _ = closed.notified() => break,
            opt = rx.recv() => {
                let Some(bytes) = opt else { break };
                if logged < 10 {
                    debug_log(
                        "H",
                        "crates/app/src/webrtc_gateway.rs:dc_to_tcp_loop",
                        "dc_write",
                        serde_json::json!({
                            "bytes": bytes.len(),
                            "idx": logged + 1,
                            "first": bytes.first().copied().unwrap_or(0)
                        }),
                    );
                    logged += 1;
                }
                if let Err(err) = tcp_w.write_all(&bytes).await {
                    debug_log(
                        "H",
                        "crates/app/src/webrtc_gateway.rs:dc_to_tcp_loop",
                        "tcp_write_err",
                        serde_json::json!({ "error": err.to_string() }),
                    );
                    return Err(err.into());
                }
            }
        }
    }
    Ok(())
}
