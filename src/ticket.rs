use serde::Deserialize;
use std::time::Duration;
use tokio::sync::Mutex;

/// Gateway #717 service-ticket plane client. Exchanges the device credential
/// for a short-lived (<= 15m) loopback ticket via POST /api/v1/link/ticket,
/// caches it until shortly before expiry, and surfaces auth failures
/// visibly (never a silent retry).
pub struct TicketManager {
    http: reqwest::Client,
    gateway_url: String,
    credential: String,
    state: Mutex<Option<CachedTicket>>,
}

struct CachedTicket {
    token: String,
    expires_at: u64,
}

// Re-validate 60s before actual expiry so mid-task expiry stays unlikely.
const EXPIRY_MARGIN_SECS: u64 = 60;

#[derive(Debug)]
pub enum TicketError {
    /// Gateway has no link plane enabled (404/link_disabled): fall back to
    /// the static gateway_token path.
    PlaneDisabled,
    /// Gateway rejected the device credential (401/invalid_credential).
    Rejected(String),
    /// Transport or 5xx failure.
    Failed(String),
}

#[derive(Debug, Deserialize)]
struct TicketResponse {
    ticket: String,
    expires_in: u64,
}

impl TicketManager {
    pub fn new(http: reqwest::Client, gateway_url: String, credential: String) -> Self {
        Self {
            http,
            gateway_url,
            credential,
            state: Mutex::new(None),
        }
    }

    pub async fn ticket(&self, task_id: &str) -> Result<String, TicketError> {
        let mut state = self.state.lock().await;
        if let Some(cached) = state.as_ref() {
            if crate::protocol::now_millis() / 1000 + EXPIRY_MARGIN_SECS < cached.expires_at {
                return Ok(cached.token.clone());
            }
            tracing::info!(task_id = %task_id, "cached link ticket expired, re-exchanging");
            *state = None;
        }

        let url = format!(
            "{}/api/v1/link/ticket",
            self.gateway_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "task_id": task_id,
                "credential": self.credential,
            }))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| TicketError::Failed(format!("ticket exchange failed: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            tracing::info!("link ticket plane not enabled on gateway (404)");
            return Err(TicketError::PlaneDisabled);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                tracing::error!(task_id = %task_id, "device credential rejected by gateway");
                return Err(TicketError::Rejected(snippet(&body)));
            }
            return Err(TicketError::Failed(format!(
                "ticket exchange http {status}: {}",
                snippet(&body)
            )));
        }

        let parsed: TicketResponse = resp
            .json()
            .await
            .map_err(|e| TicketError::Failed(format!("ticket response parse failed: {e}")))?;
        tracing::info!(task_id = %task_id, "link ticket issued (ttl {}s)", parsed.expires_in);
        *state = Some(CachedTicket {
            token: parsed.ticket.clone(),
            expires_at: crate::protocol::now_millis() / 1000 + parsed.expires_in,
        });
        Ok(parsed.ticket)
    }

    pub async fn invalidate(&self) {
        *self.state.lock().await = None;
    }
}

fn snippet(body: &str) -> String {
    body.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(url: &str) -> TicketManager {
        TicketManager::new(
            reqwest::Client::new(),
            url.to_string(),
            "device-cred-1234567890".to_string(),
        )
    }

    #[tokio::test]
    async fn exchanges_then_uses_cache() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/v1/link/ticket"))
            .and(wiremock::matchers::body_json(
                serde_json::json!({"task_id": "t-1", "credential": "device-cred-1234567890"}),
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ticket": "tok", "token_type": "bearer", "expires_in": 900, "task_id": "t-1"})),
            )
            .expect(1)
            .mount(&mock)
            .await;
        let mgr = manager(&mock.uri());
        let t1 = mgr.ticket("t-1").await.unwrap();
        let t2 = mgr.ticket("t-1").await.unwrap();
        assert_eq!(t1, "tok");
        assert_eq!(t2, "tok");
        // cache hit: exchange endpoint called exactly once
    }

    #[tokio::test]
    async fn plane_disabled_is_distinct() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        let mgr = manager(&mock.uri());
        assert!(matches!(
            mgr.ticket("t").await,
            Err(TicketError::PlaneDisabled)
        ));
    }

    #[tokio::test]
    async fn rejected_credential_surfaces() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"code": "invalid_credential", "message": "device credential rejected"})),
            )
            .mount(&mock)
            .await;
        let mgr = manager(&mock.uri());
        assert!(matches!(
            mgr.ticket("t").await,
            Err(TicketError::Rejected(_))
        ));
    }

    #[tokio::test]
    async fn invalidate_forces_reexchange() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ticket": "tok", "expires_in": 900})),
            )
            .expect(2)
            .mount(&mock)
            .await;
        let mgr = manager(&mock.uri());
        mgr.ticket("t-1").await.unwrap();
        mgr.invalidate().await;
        mgr.ticket("t-2").await.unwrap();
    }
}
