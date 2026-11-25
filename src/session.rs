use crate::backoff::Backoff;
use crate::config::Config;
use crate::gateway;
use crate::health;
use crate::keychain::Credential;
use crate::outbox::{Outbox, outbox_path};
use crate::protocol::{
    AckPayload, Envelope, ErrorCode, ErrorPayload, HeartbeatPayload, HelloPayload, MsgType,
    TaskDispatchPayload, TaskEventPayload, TaskResultPayload, TaskState,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const RECONNECT_BASE_MS: u64 = 1_000;
const RECONNECT_MAX_MS: u64 = 60_000;

pub async fn run(cfg: Config, cred: Credential, shutdown: CancellationToken) -> Result<(), String> {
    cfg.ensure_state_dir()?;
    let outbox_path = outbox_path(&cfg.state_dir);
    let mut outbox = Outbox::load(outbox_path);
    let http = health::new_client();
    let mut backoff = Backoff::new(RECONNECT_BASE_MS, RECONNECT_MAX_MS);

    info!(
        broker = %cfg.broker_url,
        device_id = %cred.device_id,
        capabilities = ?cfg.task_routes.keys().collect::<Vec<_>>(),
        "fusion-link session starting"
    );

    loop {
        if shutdown.is_cancelled() {
            info!("shutdown requested, session loop exiting");
            return Ok(());
        }
        match run_once(&cfg, &cred, &mut outbox, &http, &shutdown).await {
            SessionEnd::Shutdown => {
                info!("session ended gracefully");
                return Ok(());
            }
            SessionEnd::Disconnected(e) => {
                warn!("session disconnected: {e}; reconnecting with backoff");
                if e.contains(CONNECTED_MARK) {
                    backoff.reset();
                }
            }
        }
        let delay = backoff.next_delay();
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.cancelled() => {
                info!("shutdown during backoff, exiting");
                return Ok(());
            }
        }
    }
}

pub enum SessionEnd {
    Shutdown,
    /// Disconnected. Marker distinguishes "never connected" from
    /// "connected then dropped" so backoff only resets after real contact.
    Disconnected(String),
}

const CONNECTED_MARK: &str = "[connected]";

async fn run_once(
    cfg: &Config,
    cred: &Credential,
    outbox: &mut Outbox,
    http: &reqwest::Client,
    shutdown: &CancellationToken,
) -> SessionEnd {
    info!("connecting to broker {}", cfg.broker_url);
    let (ws, _resp) = match connect_async(&cfg.broker_url).await {
        Ok(v) => v,
        Err(e) => return SessionEnd::Disconnected(format!("ws connect failed: {e}")),
    };
    info!("broker connection established");

    let (mut sink, mut stream) = ws.split();
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<Envelope>();

    if let Err(e) = send_hello(&mut sink, cfg, cred).await {
        return SessionEnd::Disconnected(format!("{CONNECTED_MARK} hello send failed: {e}"));
    }
    if let Err(e) = replay_pending(&mut sink, outbox).await {
        return SessionEnd::Disconnected(format!("{CONNECTED_MARK} replay failed: {e}"));
    }

    let mut heartbeat =
        tokio::time::interval(std::time::Duration::from_secs(cfg.heartbeat_secs.max(1)));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First tick fires immediately; skip it (hello just sent).
    heartbeat.tick().await;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("shutdown requested, closing broker connection");
                let _ = sink.send(Message::Close(None)).await;
                return SessionEnd::Shutdown;
            }
            _ = heartbeat.tick() => {
                let result = send_heartbeat(outbox, &mut sink, http, cfg, cred).await;
                if let Err(e) = result {
                    return SessionEnd::Disconnected(format!("{CONNECTED_MARK} {e}"));
                }
            }
            Some(env) = up_rx.recv() => {
                if let Err(e) = queue_uplink(outbox, &mut sink, env).await {
                    return SessionEnd::Disconnected(format!("{CONNECTED_MARK} uplink send failed: {e}"));
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(e) = handle_text(cfg, cred, outbox, &mut sink, &up_tx, &text).await {
                            return SessionEnd::Disconnected(format!("{CONNECTED_MARK} handle failed: {e}"));
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = sink.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(frame))) => {
                        return SessionEnd::Disconnected(format!("{CONNECTED_MARK} broker closed connection: {frame:?}"));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return SessionEnd::Disconnected(format!("{CONNECTED_MARK} ws read error: {e}")),
                    None => return SessionEnd::Disconnected(format!("{CONNECTED_MARK} ws stream ended")),
                }
            }
        }
    }
}

async fn send_heartbeat(
    outbox: &mut Outbox,
    sink: &mut futures_util::stream::SplitSink<WsStream, Message>,
    http: &reqwest::Client,
    cfg: &Config,
    cred: &Credential,
) -> Result<(), String> {
    let snap = health::snapshot(http, &cfg.probes).await;
    let payload = HeartbeatPayload { health: snap };
    let value =
        serde_json::to_value(&payload).map_err(|e| format!("heartbeat serialize failed: {e}"))?;
    let env = Envelope::new(MsgType::Heartbeat, &cred.device_id, 0, value);
    queue_uplink(outbox, sink, env).await
}

async fn queue_uplink(
    outbox: &mut Outbox,
    sink: &mut futures_util::stream::SplitSink<WsStream, Message>,
    env: Envelope,
) -> Result<(), String> {
    let seqed = outbox.push(env)?;
    let raw = seqed.to_json();
    if raw.is_empty() {
        return Err("envelope serialization failed".to_string());
    }
    sink.send(Message::Text(raw))
        .await
        .map_err(|e| format!("ws send failed: {e}"))
}

async fn send_raw(
    sink: &mut futures_util::stream::SplitSink<WsStream, Message>,
    raw: String,
) -> Result<(), String> {
    sink.send(Message::Text(raw))
        .await
        .map_err(|e| format!("ws send failed: {e}"))
}

async fn send_hello(
    sink: &mut futures_util::stream::SplitSink<WsStream, Message>,
    cfg: &Config,
    cred: &Credential,
) -> Result<(), String> {
    let payload = HelloPayload {
        token: cred.token.clone(),
        fingerprint: cred.fingerprint.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: cfg.task_routes.keys().cloned().collect(),
    };
    let env = Envelope::new(
        MsgType::Hello,
        &cred.device_id,
        0,
        serde_json::to_value(&payload).map_err(|e| e.to_string())?,
    );
    send_raw(sink, env.to_json()).await
}

async fn replay_pending(
    sink: &mut futures_util::stream::SplitSink<WsStream, Message>,
    outbox: &mut Outbox,
) -> Result<(), String> {
    let pending: Vec<Envelope> = outbox.pending().to_vec();
    if pending.is_empty() {
        return Ok(());
    }
    info!("replaying {} pending uplink messages", pending.len());
    for env in pending {
        send_raw(sink, env.to_json()).await?;
    }
    Ok(())
}

async fn handle_text(
    cfg: &Config,
    cred: &Credential,
    outbox: &mut Outbox,
    sink: &mut futures_util::stream::SplitSink<WsStream, Message>,
    up_tx: &mpsc::UnboundedSender<Envelope>,
    text: &str,
) -> Result<(), String> {
    let env = match Envelope::from_json(text) {
        Ok(e) => e,
        Err(e) => {
            warn!("dropping malformed broker message: {e}");
            return Ok(());
        }
    };
    match env.msg_type {
        MsgType::HelloAck => {
            info!("broker accepted registration (hello_ack)");
        }
        MsgType::Ack => match serde_json::from_value::<AckPayload>(env.payload) {
            Ok(ack) => {
                outbox.ack(ack.ack_seq)?;
            }
            Err(e) => warn!("malformed ack payload: {e}"),
        },
        MsgType::TaskDispatch => {
            let payload: TaskDispatchPayload = match serde_json::from_value(env.payload) {
                Ok(p) => p,
                Err(e) => {
                    error!("malformed task.dispatch payload: {e}");
                    return Ok(());
                }
            };
            info!(task_id = %payload.task_id, task_type = %payload.task_type, "task dispatch received");
            // Ack the downlink so broker knows delivery succeeded.
            let ack = Envelope::new(
                MsgType::Ack,
                &cred.device_id,
                0,
                serde_json::to_value(AckPayload { ack_seq: env.seq }).map_err(|e| e.to_string())?,
            );
            send_raw(sink, ack.to_json()).await?;
            spawn_task_runner(
                cfg.clone(),
                cred.clone(),
                payload,
                http_client(),
                up_tx.clone(),
            );
        }
        MsgType::ConfigUpdate => {
            info!(
                seq = env.seq,
                "config.update received (apply is a no-op in M1)"
            );
        }
        MsgType::Error => match serde_json::from_value::<ErrorPayload>(env.payload) {
            Ok(err) => error!(code = ?err.code, "broker error: {}", err.message),
            Err(e) => warn!("malformed error payload: {e}"),
        },
        _ => {
            tracing::debug!(msg_type = ?env.msg_type, "ignoring unexpected message type");
        }
    }
    Ok(())
}

fn http_client() -> Arc<reqwest::Client> {
    Arc::new(health::new_client())
}

fn spawn_task_runner(
    cfg: Config,
    cred: Credential,
    dispatch: TaskDispatchPayload,
    http: Arc<reqwest::Client>,
    up_tx: mpsc::UnboundedSender<Envelope>,
) {
    tokio::spawn(async move {
        let task_id = dispatch.task_id.clone();
        emit_event(&cred, &up_tx, &task_id, TaskState::Running, None, None);

        match gateway::execute(&http, &cfg, &dispatch).await {
            Ok(outcome) => {
                emit_event(&cred, &up_tx, &task_id, outcome.state, None, None);
                let payload = TaskResultPayload {
                    task_id: task_id.clone(),
                    state: outcome.state,
                    summary: outcome.summary,
                    artifact_refs: outcome.artifact_refs,
                };
                send_uplink(&cred, &up_tx, MsgType::TaskResult, payload);
            }
            Err(gateway::TaskError::Unsupported(reason)) => {
                emit_event(
                    &cred,
                    &up_tx,
                    &task_id,
                    TaskState::Failed,
                    None,
                    Some(reason.clone()),
                );
                send_uplink(
                    &cred,
                    &up_tx,
                    MsgType::Error,
                    ErrorPayload {
                        code: ErrorCode::Unsupported,
                        message: reason,
                    },
                );
            }
            Err(gateway::TaskError::LocalFail(reason)) => {
                emit_event(
                    &cred,
                    &up_tx,
                    &task_id,
                    TaskState::Failed,
                    None,
                    Some(reason.clone()),
                );
                send_uplink(
                    &cred,
                    &up_tx,
                    MsgType::Error,
                    ErrorPayload {
                        code: ErrorCode::LocalFail,
                        message: reason,
                    },
                );
            }
        }
        info!(task_id = %task_id, "task runner finished");
    });
}

fn emit_event(
    cred: &Credential,
    up_tx: &mpsc::UnboundedSender<Envelope>,
    task_id: &str,
    state: TaskState,
    progress: Option<u8>,
    detail: Option<String>,
) {
    send_uplink(
        cred,
        up_tx,
        MsgType::TaskEvent,
        TaskEventPayload {
            task_id: task_id.to_string(),
            state,
            progress,
            detail,
        },
    );
}

fn send_uplink<T: serde::Serialize>(
    cred: &Credential,
    up_tx: &mpsc::UnboundedSender<Envelope>,
    msg_type: MsgType,
    payload: T,
) {
    match serde_json::to_value(&payload) {
        Ok(value) => {
            let env = Envelope::new(msg_type, &cred.device_id, 0, value);
            if let Err(e) = up_tx.send(env) {
                error!("uplink channel closed, event dropped: {e}");
            }
        }
        Err(e) => error!("uplink payload serialize failed: {e}"),
    }
}
