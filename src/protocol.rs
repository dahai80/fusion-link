use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MsgType {
    #[serde(rename = "hello")]
    Hello,
    #[serde(rename = "hello_ack")]
    HelloAck,
    #[serde(rename = "heartbeat")]
    Heartbeat,
    #[serde(rename = "task.dispatch")]
    TaskDispatch,
    #[serde(rename = "task.event")]
    TaskEvent,
    #[serde(rename = "task.result")]
    TaskResult,
    #[serde(rename = "artifact.index")]
    ArtifactIndex,
    #[serde(rename = "config.update")]
    ConfigUpdate,
    #[serde(rename = "ack")]
    Ack,
    #[serde(rename = "error")]
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    #[serde(rename = "E_AUTH")]
    Auth,
    #[serde(rename = "E_QUOTA")]
    Quota,
    #[serde(rename = "E_OFFLINE")]
    Offline,
    #[serde(rename = "E_UNSUPPORTED")]
    Unsupported,
    #[serde(rename = "E_LOCAL_FAIL")]
    LocalFail,
    // FR-8 (issue #13): local confirmation point denied or timed out.
    #[serde(rename = "E_DENIED")]
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    AwaitingLocalConfirm,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    #[serde(rename = "type")]
    pub msg_type: MsgType,
    pub ts: u64,
    pub device_id: String,
    pub seq: u64,
    pub payload: Value,
}

impl Envelope {
    pub fn new(msg_type: MsgType, device_id: &str, seq: u64, payload: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            msg_type,
            ts: now_millis(),
            device_id: device_id.to_string(),
            seq,
            payload,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            tracing::error!("envelope serialize failed, dropping message");
            String::new()
        })
    }

    pub fn from_json(raw: &str) -> Result<Self, String> {
        serde_json::from_str(raw).map_err(|e| format!("envelope parse failed: {e}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloPayload {
    pub token: String,
    pub fingerprint: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    pub health: HealthSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub services: Vec<ServiceHealth>,
    pub generated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceHealth {
    pub name: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDispatchPayload {
    pub task_id: String,
    /// Broker (fusion-ops) sends this as "type"; PRD draft said "task_type".
    /// Accept both, keep "task_type" on the wire for our own uplink frames.
    #[serde(alias = "type")]
    pub task_type: String,
    pub params: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEventPayload {
    pub task_id: String,
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResultPayload {
    pub task_id: String,
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckPayload {
    pub ack_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: ErrorCode,
    /// Broker (fusion-ops) uses "detail" on the wire; PRD draft said "message".
    /// Unified on "detail" in both directions, "message" accepted on parse.
    #[serde(rename = "detail", alias = "message")]
    pub message: String,
}

pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip_preserves_type_and_payload() {
        let payload = TaskDispatchPayload {
            task_id: "t-1".into(),
            task_type: "shell.chat".into(),
            params: serde_json::json!({"prompt": "hi"}),
            ttl_secs: Some(300),
        };
        let env = Envelope::new(
            MsgType::TaskDispatch,
            "dev-1",
            7,
            serde_json::to_value(&payload).unwrap(),
        );
        let raw = env.to_json();
        let back = Envelope::from_json(&raw).unwrap();
        assert_eq!(back.msg_type, MsgType::TaskDispatch);
        assert_eq!(back.seq, 7);
        assert_eq!(back.device_id, "dev-1");
        let parsed: TaskDispatchPayload = serde_json::from_value(back.payload).unwrap();
        assert_eq!(parsed.task_type, "shell.chat");
    }

    #[test]
    fn error_code_serializes_to_prd_names() {
        let payload = ErrorPayload {
            code: ErrorCode::Unsupported,
            message: "no route".into(),
        };
        let raw = serde_json::to_string(&payload).unwrap();
        assert!(raw.contains("E_UNSUPPORTED"), "raw={raw}");
    }

    #[test]
    fn task_state_serializes_snake_case() {
        let raw = serde_json::to_string(&TaskState::AwaitingLocalConfirm).unwrap();
        assert_eq!(raw, "\"awaiting_local_confirm\"");
    }

    #[test]
    fn malformed_json_rejected() {
        assert!(Envelope::from_json("{not json").is_err());
    }

    #[test]
    fn dispatch_accepts_broker_type_field() {
        // fusion-ops broker sends {"task_id", "type", "params"}
        let raw = r#"{"task_id":"t-9","type":"summarize","params":{}}"#;
        let p: TaskDispatchPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(p.task_type, "summarize");
    }

    #[test]
    fn error_payload_unified_on_detail() {
        let payload = ErrorPayload {
            code: ErrorCode::Quota,
            message: "over quota".into(),
        };
        let raw = serde_json::to_string(&payload).unwrap();
        assert!(raw.contains("\"detail\""), "serialize uses detail: {raw}");
        assert!(
            !raw.contains("\"message\""),
            "no message key on wire: {raw}"
        );
        // broker frame {"code","detail"} parses
        let back: ErrorPayload =
            serde_json::from_str(r#"{"code":"E_QUOTA","detail":"over quota"}"#).unwrap();
        assert_eq!(back.message, "over quota");
        // legacy {"code","message"} also parses
        let legacy: ErrorPayload =
            serde_json::from_str(r#"{"code":"E_QUOTA","message":"over quota"}"#).unwrap();
        assert_eq!(legacy.message, "over quota");
    }
}
