use fusion_link::config::{Config, TaskRoute};
use fusion_link::keychain::Credential;
use fusion_link::protocol::{
    AckPayload, Envelope, MsgType, TaskDispatchPayload, TaskEventPayload, TaskResultPayload,
};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

struct BrokerReport {
    uplink: Vec<Envelope>,
    downlink_acked: Option<u64>,
}

/// Mock broker: accepts one device connection, reads hello, sends hello_ack,
/// dispatches one task, collects uplink until task.result, waits for a
/// heartbeat, acks everything, then closes.
async fn spawn_mock_broker(
    dispatch: TaskDispatchPayload,
    dispatch_seq: u64,
) -> (String, tokio::task::JoinHandle<BrokerReport>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let (mut sink, mut stream) = ws.split();

        // hello
        let hello = read_envelope(&mut stream).await;
        assert_eq!(
            hello.msg_type,
            MsgType::Hello,
            "first message must be hello"
        );

        // hello_ack
        let ack = Envelope::new(MsgType::HelloAck, "broker", 0, serde_json::json!({}));
        sink.send(Message::Text(ack.to_json())).await.unwrap();

        // task.dispatch
        let dispatch_env = Envelope::new(
            MsgType::TaskDispatch,
            "broker",
            dispatch_seq,
            serde_json::to_value(&dispatch).unwrap(),
        );
        sink.send(Message::Text(dispatch_env.to_json()))
            .await
            .unwrap();

        // collect uplink until task.result, then wait for a heartbeat
        let mut uplink: Vec<Envelope> = Vec::new();
        let mut downlink_acked: Option<u64> = None;
        let mut saw_result = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            assert!(tokio::time::Instant::now() < deadline, "broker timed out");
            let env = tokio::time::timeout(Duration::from_secs(15), read_envelope(&mut stream))
                .await
                .expect("no message from link in time");
            match env.msg_type {
                MsgType::Ack => {
                    let payload: AckPayload = serde_json::from_value(env.payload).unwrap();
                    downlink_acked = Some(payload.ack_seq);
                }
                MsgType::TaskResult => {
                    saw_result = true;
                    uplink.push(env);
                }
                MsgType::Heartbeat => {
                    if saw_result {
                        uplink.push(env);
                        break;
                    }
                    uplink.push(env);
                }
                _ => uplink.push(env),
            }
        }
        // ack all uplink so the outbox drains
        let max_seq = uplink.iter().map(|e| e.seq).max().unwrap_or(0);
        let ack_env = Envelope::new(
            MsgType::Ack,
            "broker",
            0,
            serde_json::to_value(AckPayload { ack_seq: max_seq }).unwrap(),
        );
        sink.send(Message::Text(ack_env.to_json())).await.unwrap();
        // give the link a moment to process the ack before dropping the socket
        tokio::time::sleep(Duration::from_millis(300)).await;
        BrokerReport {
            uplink,
            downlink_acked,
        }
    });
    (format!("ws://{addr}"), handle)
}

async fn read_envelope(
    stream: &mut futures_util::stream::SplitStream<WebSocketStream<tokio::net::TcpStream>>,
) -> Envelope {
    loop {
        let msg = stream.next().await.expect("stream ended").unwrap();
        if let Message::Text(text) = msg {
            return Envelope::from_json(&text).expect("broker got non-envelope text");
        }
    }
}

fn test_config(broker_url: &str, gateway_url: &str, state_dir: std::path::PathBuf) -> Config {
    let mut routes = HashMap::new();
    routes.insert(
        "shell.chat".to_string(),
        TaskRoute {
            method: "POST".to_string(),
            path: "/api/v1/shell/chat/completions".to_string(),
            timeout_secs: 10,
        },
    );
    Config {
        broker_url: broker_url.to_string(),
        bind_url: "http://127.0.0.1:1/bind".to_string(),
        gateway_url: gateway_url.to_string(),
        gateway_token: None,
        heartbeat_secs: 1,
        state_dir,
        task_routes: routes,
        probes: Vec::new(),
    }
}

#[tokio::test]
async fn full_session_dispatch_progress_result_and_outbox_drain() {
    // mock local gateway
    let gateway_mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v1/shell/chat/completions"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"content": "task done"})),
        )
        .mount(&gateway_mock)
        .await;

    let dispatch = TaskDispatchPayload {
        task_id: "task-e2e-1".to_string(),
        task_type: "shell.chat".to_string(),
        params: serde_json::json!({"prompt": "hello"}),
        ttl_secs: None,
    };
    let (broker_url, broker_handle) = spawn_mock_broker(dispatch, 42).await;

    let state_dir = tempfile::tempdir().unwrap();
    let cfg = test_config(
        &broker_url,
        &gateway_mock.uri(),
        state_dir.path().to_path_buf(),
    );
    let cred = Credential {
        device_id: "test-device".to_string(),
        token: "test-token".to_string(),
        fingerprint: "fp-test".to_string(),
    };

    let token = tokio_util::sync::CancellationToken::new();
    let session_task = tokio::spawn(session_run(cfg, cred, token.clone()));

    let report = broker_handle.await.unwrap();

    // downlink dispatch was acked with the broker's seq
    assert_eq!(report.downlink_acked, Some(42));

    // progress events include running -> succeeded plus a result
    let states: Vec<MsgType> = report.uplink.iter().map(|e| e.msg_type).collect();
    assert!(
        states.contains(&MsgType::TaskEvent),
        "no task.event: {states:?}"
    );
    assert!(
        states.contains(&MsgType::TaskResult),
        "no task.result: {states:?}"
    );
    assert!(
        states.contains(&MsgType::Heartbeat),
        "no heartbeat: {states:?}"
    );

    let events: Vec<TaskEventPayload> = report
        .uplink
        .iter()
        .filter(|e| e.msg_type == MsgType::TaskEvent)
        .map(|e| serde_json::from_value(e.payload.clone()).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e.state == fusion_link::protocol::TaskState::Running)
    );
    assert!(
        events
            .iter()
            .any(|e| e.state == fusion_link::protocol::TaskState::Succeeded)
    );

    let result: TaskResultPayload = report
        .uplink
        .iter()
        .find(|e| e.msg_type == MsgType::TaskResult)
        .map(|e| serde_json::from_value(e.payload.clone()).unwrap())
        .unwrap();
    assert_eq!(result.task_id, "task-e2e-1");
    assert_eq!(result.summary.as_deref(), Some("task done"));

    // uplink envelopes were seq-numbered
    let mut seqs: Vec<u64> = report.uplink.iter().map(|e| e.seq).collect();
    seqs.sort();
    assert_eq!(seqs.first(), Some(&0), "seqs must start at 0: {seqs:?}");

    // session ends cleanly on shutdown
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), session_task).await;

    // outbox drained: broker acked everything
    let outbox = fusion_link::outbox::Outbox::load(state_dir.path().join("outbox.json"));
    assert_eq!(
        outbox.pending_count(),
        0,
        "outbox should be empty after acks"
    );
}

// helper so the spawn above stays tokio-unaware of shutdown plumbing details
async fn session_run(
    cfg: Config,
    cred: Credential,
    token: tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    fusion_link::session::run(cfg, cred, token).await
}
