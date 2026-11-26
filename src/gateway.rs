use crate::config::Config;
use crate::protocol::{TaskDispatchPayload, TaskState};
use base64::Engine as _;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug)]
pub struct TaskOutcome {
    pub state: TaskState,
    pub summary: Option<String>,
    pub artifact_refs: Vec<Value>,
}

#[derive(Debug)]
pub enum TaskError {
    Unsupported(String),
    LocalFail(String),
    /// Gateway rejected auth (401). Per #717 semantics: visible failure,
    /// never a silent retry.
    Auth(String),
}

pub async fn execute(
    client: &reqwest::Client,
    cfg: &Config,
    dispatch: &TaskDispatchPayload,
    bearer: Option<&str>,
) -> Result<TaskOutcome, TaskError> {
    let route = cfg.task_routes.get(&dispatch.task_type).ok_or_else(|| {
        tracing::warn!("unsupported task type: {}", dispatch.task_type);
        TaskError::Unsupported(format!(
            "task type '{}' not supported by this device",
            dispatch.task_type
        ))
    })?;

    let url = format!("{}{}", cfg.gateway_url.trim_end_matches('/'), route.path);
    let method = route.method.to_uppercase();
    let timeout = Duration::from_secs(if route.timeout_secs > 0 {
        route.timeout_secs
    } else {
        300
    });

    tracing::info!(
        task_id = %dispatch.task_id,
        task_type = %dispatch.task_type,
        url = %url,
        "dispatching task to local gateway"
    );

    // Multipart contract (issue #11): when params carry `_multipart: true`,
    // send a multipart/form-data body instead of JSON — required by
    // office-api endpoints that take UploadFile / Form inputs.
    // - `file_b64` (string, base64) becomes the `file` part,
    //   named by `file_name` (default "file.bin"), typed by `file_mime`
    //   (default application/octet-stream).
    // - Every other param becomes a form field (non-strings JSON-encoded).
    let multipart = dispatch
        .params
        .get("_multipart")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if multipart && method != "POST" {
        return Err(TaskError::Unsupported(format!(
            "multipart body requires POST, route method is '{}'",
            route.method
        )));
    }

    let mut req = match method.as_str() {
        "GET" => client.get(&url),
        "POST" => {
            if multipart {
                let form = build_form(&dispatch.params)?;
                client.post(&url).multipart(form)
            } else {
                client.post(&url).json(&dispatch.params)
            }
        }
        "PUT" => client.put(&url).json(&dispatch.params),
        _ => {
            return Err(TaskError::Unsupported(format!(
                "route method '{}' not supported",
                route.method
            )));
        }
    };
    // Auth precedence: session ticket (link plane) > static gateway_token.
    let token = bearer.or(cfg.gateway_token.as_deref());
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }

    match req.timeout(timeout).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                tracing::error!(task_id = %dispatch.task_id, "gateway rejected auth (401)");
                return Err(TaskError::Auth("gateway auth rejected (401)".into()));
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                let snippet: String = body.chars().take(200).collect();
                tracing::error!(task_id = %dispatch.task_id, "gateway returned http error: status={status} body={snippet}");
                return Err(TaskError::LocalFail(format!("gateway http {status}")));
            }
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            tracing::info!(task_id = %dispatch.task_id, "gateway task completed successfully");
            Ok(TaskOutcome {
                state: TaskState::Succeeded,
                summary: Some(summarize(&body)),
                artifact_refs: Vec::new(),
            })
        }
        Err(e) => {
            tracing::error!(task_id = %dispatch.task_id, "gateway request failed: {e}");
            Err(TaskError::LocalFail(format!("gateway unreachable: {e}")))
        }
    }
}

const META_KEYS: [&str; 4] = ["_multipart", "file_b64", "file_name", "file_mime"];

fn build_form(params: &Value) -> Result<reqwest::multipart::Form, TaskError> {
    let mut form = reqwest::multipart::Form::new();
    if let Some(obj) = params.as_object() {
        if let Some(b64) = obj.get("file_b64").and_then(Value::as_str) {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    tracing::error!("file_b64 decode failed: {e}");
                    TaskError::LocalFail(format!("file_b64 is not valid base64: {e}"))
                })?;
            let name = obj
                .get("file_name")
                .and_then(Value::as_str)
                .unwrap_or("file.bin");
            let mime = obj
                .get("file_mime")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream");
            form = form.part(
                "file",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(name.to_string())
                    .mime_str(mime)
                    .map_err(|e| TaskError::LocalFail(format!("invalid file_mime: {e}")))?,
            );
        }
        for (key, value) in obj {
            if META_KEYS.contains(&key.as_str()) {
                continue;
            }
            let text = match value {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            form = form.text(key.clone(), text);
        }
    }
    Ok(form)
}

fn summarize(body: &Value) -> String {
    match body {
        Value::String(s) => s.chars().take(200).collect(),
        Value::Object(map) => {
            for key in ["summary", "content", "message", "text", "id"] {
                if let Some(v) = map.get(key)
                    && let Some(s) = v.as_str()
                {
                    return s.chars().take(200).collect();
                }
            }
            serde_json::to_string(body)
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect()
        }
        other => serde_json::to_string(other)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TaskRoute;
    use std::collections::HashMap;

    fn test_cfg(url: &str) -> Config {
        let mut routes = HashMap::new();
        routes.insert(
            "shell.chat".to_string(),
            TaskRoute {
                method: "POST".to_string(),
                path: "/api/v1/shell/chat/completions".to_string(),
                timeout_secs: 5,
            },
        );
        routes.insert(
            "office.spreadsheet".to_string(),
            TaskRoute {
                method: "POST".to_string(),
                path: "/shell/office-api/v1/xlsx/parse".to_string(),
                timeout_secs: 5,
            },
        );
        routes.insert(
            "office.document".to_string(),
            TaskRoute {
                method: "POST".to_string(),
                path: "/shell/office-api/v1/docx/write".to_string(),
                timeout_secs: 5,
            },
        );
        Config {
            gateway_url: url.to_string(),
            task_routes: routes,
            ..Default::default()
        }
    }

    fn dispatch(task_type: &str) -> TaskDispatchPayload {
        TaskDispatchPayload {
            task_id: "t-1".into(),
            task_type: task_type.into(),
            params: serde_json::json!({"prompt": "hi"}),
            ttl_secs: None,
        }
    }

    #[tokio::test]
    async fn unsupported_type_rejected_without_http_call() {
        let mock = wiremock::MockServer::start().await;
        let cfg = test_cfg(&mock.uri());
        let err = execute(&client(), &cfg, &dispatch("no.such.type"), None)
            .await
            .unwrap_err();
        match err {
            TaskError::Unsupported(msg) => assert!(msg.contains("no.such.type")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert_eq!(mock.received_requests().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn success_returns_summary() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/v1/shell/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"content": "answer text"})),
            )
            .mount(&mock)
            .await;
        let cfg = test_cfg(&mock.uri());
        let out = execute(&client(), &cfg, &dispatch("shell.chat"), None)
            .await
            .unwrap();
        assert_eq!(out.state, TaskState::Succeeded);
        assert_eq!(out.summary.as_deref(), Some("answer text"));
    }

    #[tokio::test]
    async fn gateway_http_error_maps_to_local_fail() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&mock)
            .await;
        let cfg = test_cfg(&mock.uri());
        match execute(&client(), &cfg, &dispatch("shell.chat"), None).await {
            Err(TaskError::LocalFail(msg)) => assert!(msg.contains("500"), "{msg}"),
            other => panic!("expected LocalFail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gateway_down_maps_to_local_fail() {
        let cfg = test_cfg("http://127.0.0.1:1");
        match execute(&client(), &cfg, &dispatch("shell.chat"), None).await {
            Err(TaskError::LocalFail(_)) => {}
            other => panic!("expected LocalFail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bearer_token_attached_when_configured() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::header("authorization", "Bearer tok-1"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&mock)
            .await;
        let mut cfg = test_cfg(&mock.uri());
        cfg.gateway_token = Some("tok-1".into());
        execute(&client(), &cfg, &dispatch("shell.chat"), None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn session_ticket_takes_precedence_over_gateway_token() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer ticket-9",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock)
            .await;
        let mut cfg = test_cfg(&mock.uri());
        cfg.gateway_token = Some("static-token".into());
        execute(&client(), &cfg, &dispatch("shell.chat"), Some("ticket-9"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn gateway_401_maps_to_auth() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(401).set_body_json(
                    serde_json::json!({"code": "E_AUTH", "detail": "ticket expired"}),
                ),
            )
            .mount(&mock)
            .await;
        let cfg = test_cfg(&mock.uri());
        match execute(&client(), &cfg, &dispatch("shell.chat"), Some("expired")).await {
            Err(TaskError::Auth(msg)) => assert!(msg.contains("401"), "{msg}"),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multipart_body_sends_file_and_fields() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"content": "parsed"})),
            )
            .mount(&mock)
            .await;
        let cfg = test_cfg(&mock.uri());
        let mut d = dispatch("office.spreadsheet");
        d.params = serde_json::json!({
            "_multipart": true,
            "file_b64": base64::engine::general_purpose::STANDARD.encode(b"PK\x03\x04sheet"),
            "file_name": "book.xlsx",
            "file_mime": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "mode": "full",
            "sheet": "",
        });
        let out = execute(&client(), &cfg, &d, Some("tk")).await.unwrap();
        assert_eq!(out.summary.as_deref(), Some("parsed"));
        let reqs = mock.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        let ctype = reqs[0]
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(ctype.starts_with("multipart/form-data"), "{ctype}");
        let body = String::from_utf8_lossy(&reqs[0].body).to_string();
        assert!(
            body.contains("name=\"file\"; filename=\"book.xlsx\""),
            "{body}"
        );
        assert!(body.contains("name=\"mode\""), "{body}");
        // meta keys must not leak into form fields
        assert!(!body.contains("_multipart"), "{body}");
    }

    #[tokio::test]
    async fn multipart_without_file_only_form_fields() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&mock)
            .await;
        let cfg = test_cfg(&mock.uri());
        let mut d = dispatch("office.document");
        d.params = serde_json::json!({"_multipart": true, "markdown": "# hello"});
        execute(&client(), &cfg, &d, None).await.unwrap();
        let reqs = mock.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&reqs[0].body).to_string();
        assert!(body.contains("name=\"markdown\""), "{body}");
        assert!(!body.contains("name=\"file\""), "{body}");
    }

    #[tokio::test]
    async fn multipart_invalid_base64_maps_to_local_fail() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&mock)
            .await;
        let cfg = test_cfg(&mock.uri());
        let mut d = dispatch("office.spreadsheet");
        d.params = serde_json::json!({"_multipart": true, "file_b64": "!!!not-base64!!!"});
        match execute(&client(), &cfg, &d, None).await {
            Err(TaskError::LocalFail(msg)) => assert!(msg.contains("base64"), "{msg}"),
            other => panic!("expected LocalFail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multipart_on_get_rejected() {
        let mock = wiremock::MockServer::start().await;
        let mut cfg = test_cfg(&mock.uri());
        cfg.task_routes.get_mut("shell.chat").unwrap().method = "GET".into();
        let mut d = dispatch("shell.chat");
        d.params = serde_json::json!({"_multipart": true});
        match execute(&client(), &cfg, &d, None).await {
            Err(TaskError::Unsupported(msg)) => assert!(msg.contains("POST"), "{msg}"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[test]
    fn summarize_prefers_known_keys() {
        let body = serde_json::json!({"id": "x1", "content": "main text"});
        assert_eq!(summarize(&body), "main text");
    }
}
