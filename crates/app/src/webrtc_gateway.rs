use std::{collections::HashMap, convert::Infallible, net::IpAddr, sync::Arc, time::Duration};

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::{StreamExt, stream};
use ice::udp_network::{EphemeralUDP, UDPNetwork};
use serde::{Deserialize, Serialize};
use serde_json::to_string;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex, Notify, broadcast, mpsc},
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

use crate::metrics::{Metrics, MetricsSnapshot};

const DEFAULT_STUN_SERVER: &str = "stun:stun.l.google.com:19302";
const DEFAULT_SESSION_OWNER: &str = "local";
const METRICS_BUFFER: usize = 64;
const CANDIDATE_BUFFER: usize = 64;

#[derive(Clone)]
pub struct AppState {
    sessions: Arc<Mutex<HashMap<Uuid, Session>>>,
    metrics: Metrics,
    metrics_tx: broadcast::Sender<MetricsSnapshot>,
}

impl Default for AppState {
    fn default() -> Self {
        let (metrics_tx, _rx) = broadcast::channel(METRICS_BUFFER);
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            metrics: Metrics::from_env(),
            metrics_tx,
        }
    }
}

impl AppState {
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn metrics_subscribe(&self) -> broadcast::Receiver<MetricsSnapshot> {
        self.metrics_tx.subscribe()
    }

    pub fn publish_metrics(&self, snapshot: MetricsSnapshot) {
        let _ = self.metrics_tx.send(snapshot);
    }
}

struct Session {
    pc: Arc<RTCPeerConnection>,
    pending_candidates: Arc<Mutex<Vec<TrickleCandidate>>>,
    candidate_tx: broadcast::Sender<TrickleCandidate>,
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

#[derive(Deserialize)]
pub struct StreamRequest {
    pub session_id: String,
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
    let pending_candidates: Arc<Mutex<Vec<TrickleCandidate>>> = Arc::new(Mutex::new(Vec::new()));
    let (candidate_tx, _candidate_rx) = broadcast::channel(CANDIDATE_BUFFER);
    let session_owner = current_session_owner();

    let pc = create_peer_connection(pending_candidates.clone(), candidate_tx.clone())
        .await
        .map_err(internal_error)?;

    pc.set_remote_description(RTCSessionDescription::offer(req.sdp).map_err(internal_error)?)
        .await
        .map_err(internal_error)?;

    let answer = pc.create_answer(None).await.map_err(internal_error)?;
    pc.set_local_description(answer.clone())
        .await
        .map_err(internal_error)?;

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            session_uuid,
            Session {
                pc: pc.clone(),
                pending_candidates,
                candidate_tx,
            },
        );
    }
    register_session_cleanup(session_uuid, state.sessions.clone(), pc.clone());

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
    let current_owner = current_session_owner();
    let (owner, session_uuid) = decode_session_id(&req.session_id, &current_owner)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "invalid session_id".into()))?;

    if owner != current_owner {
        return Ok(fly_replay_response(&owner));
    }

    let pc = {
        let sessions = state.sessions.lock().await;
        let Some(sess) = sessions.get(&session_uuid) else {
            return Err((StatusCode::NOT_FOUND, "unknown session".into()));
        };
        sess.pc.clone()
    };

    if let Some(c) = req.candidate {
        pc.add_ice_candidate(from_trickle(c))
            .await
            .map_err(internal_error)?;
    }

    Ok(Json(CandidateResponse {
        candidates: Vec::new(),
    })
    .into_response())
}

pub async fn stream_handler(
    State(state): State<AppState>,
    Query(req): Query<StreamRequest>,
) -> Result<Response, (StatusCode, String)> {
    let current_owner = current_session_owner();
    let (owner, session_uuid) = decode_session_id(&req.session_id, &current_owner)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "invalid session_id".into()))?;

    if owner != current_owner {
        return Ok(fly_replay_response(&owner));
    }

    let (pending_candidates, candidate_tx) = {
        let sessions = state.sessions.lock().await;
        let Some(sess) = sessions.get(&session_uuid) else {
            return Err((StatusCode::NOT_FOUND, "unknown session".into()));
        };
        (sess.pending_candidates.clone(), sess.candidate_tx.clone())
    };

    let initial = drain_pending_candidates(pending_candidates).await;
    let initial_stream = stream::iter(initial.into_iter().map(|cand| {
        let data = to_string(&cand).unwrap_or_else(|_| "{}".to_string());
        Ok::<Event, Infallible>(Event::default().data(data))
    }));

    let rx = candidate_tx.subscribe();
    let updates = stream::unfold(rx, |mut rx| async {
        loop {
            match rx.recv().await {
                Ok(cand) => {
                    let data = to_string(&cand).unwrap_or_else(|_| "{}".to_string());
                    let event = Event::default().data(data);
                    return Some((Ok::<Event, Infallible>(event), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    let stream = initial_stream.chain(updates);
    let res = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response();
    Ok(res)
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
    pending_candidates: Arc<Mutex<Vec<TrickleCandidate>>>,
    candidate_tx: broadcast::Sender<TrickleCandidate>,
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

    register_pending_candidate_handler(pc.clone(), pending_candidates, candidate_tx);
    register_vnc_channel(pc.clone());

    Ok(pc)
}

async fn bridge_vnc(dc: Arc<RTCDataChannel>, closed: Arc<Notify>) -> anyhow::Result<()> {
    let stream = TcpStream::connect("127.0.0.1:5900").await?;
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

    let read_task = tokio::spawn(tcp_to_dc_loop(tcp_r, dc.clone(), closed.clone()));

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
    pending_candidates: Arc<Mutex<Vec<TrickleCandidate>>>,
) -> Vec<TrickleCandidate> {
    let mut q = pending_candidates.lock().await;
    q.drain(..).collect()
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

fn register_pending_candidate_handler(
    pc: Arc<RTCPeerConnection>,
    pending_candidates: Arc<Mutex<Vec<TrickleCandidate>>>,
    candidate_tx: broadcast::Sender<TrickleCandidate>,
) {
    pc.on_ice_candidate(Box::new(move |cand| {
        let q = pending_candidates.clone();
        let candidate_tx = candidate_tx.clone();
        Box::pin(async move {
            if let Some(cand) = cand {
                if let Ok(json) = cand.to_json() {
                    let trickle = to_trickle(json);
                    if candidate_tx.receiver_count() == 0 {
                        q.lock().await.push(trickle.clone());
                    }
                    let _ = candidate_tx.send(trickle);
                }
            }
        })
    }));
}

fn register_vnc_channel(pc: Arc<RTCPeerConnection>) {
    pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
        Box::pin(async move {
            if dc.label() != "vnc" {
                return;
            }

            let closed_notify = Arc::new(Notify::new());
            register_dc_close(&dc, closed_notify.clone());
            register_dc_open(&dc, closed_notify);
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

fn register_dc_open(dc: &Arc<RTCDataChannel>, closed_notify: Arc<Notify>) {
    let dc_for_open = dc.clone();
    dc.on_open(Box::new(move || {
        let dc = dc_for_open.clone();
        let closed = closed_notify.clone();
        Box::pin(async move {
            tokio::spawn(async move {
                if let Err(err) = bridge_vnc(dc, closed).await {
                    eprintln!("VNC bridge error: {err}");
                }
            });
        })
    }));
}

fn register_session_cleanup(
    session_id: Uuid,
    sessions: Arc<Mutex<HashMap<Uuid, Session>>>,
    pc: Arc<RTCPeerConnection>,
) {
    pc.on_peer_connection_state_change(Box::new(move |state| {
        let sessions = sessions.clone();
        Box::pin(async move {
            if matches!(
                state,
                RTCPeerConnectionState::Closed
                    | RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Disconnected
            ) {
                let mut sessions = sessions.lock().await;
                sessions.remove(&session_id);
            }
        })
    }));
}

async fn tcp_to_dc_loop(
    mut tcp_r: OwnedReadHalf,
    dc: Arc<RTCDataChannel>,
    closed: Arc<Notify>,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            _ = closed.notified() => break,
            res = tcp_r.read(&mut buf) => {
                let n = res?;
                if n == 0 {
                    break;
                }
                let bytes = Bytes::copy_from_slice(&buf[..n]);
                let _ = dc.send(&bytes).await?;
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
    loop {
        tokio::select! {
            _ = closed.notified() => break,
            opt = rx.recv() => {
                let Some(bytes) = opt else { break };
                tcp_w.write_all(&bytes).await?;
            }
        }
    }
    Ok(())
}
