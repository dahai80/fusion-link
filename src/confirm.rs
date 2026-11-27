use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// FR-8 local confirmation point (issue #13): tasks that perform write
// operations pause before dispatch. The daemon posts a macOS notification
// (best effort) and waits for a decision; headless approval goes through
// `fusion-link confirm <task_id> approve|deny`, which writes a decision
// file into the shared state dir. Cloud never approves — status only.

/// Detect confirm-required tasks: the console dispatches `confirm: "local"`
/// for write-ops; `confirm_required: true` is the explicit device-side form.
pub fn requires_confirm(params: &serde_json::Value) -> bool {
    params
        .get("confirm_required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || params.get("confirm").and_then(|v| v.as_str()) == Some("local")
}

/// Task ids become confirm file names; broker-supplied ids must not
/// traverse paths.
pub fn valid_task_id(task_id: &str) -> bool {
    !task_id.is_empty()
        && !task_id.contains('/')
        && !task_id.contains('\\')
        && !task_id.contains("..")
}

pub struct Decision {
    pub approved: bool,
    pub reason: String,
}

pub fn confirm_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("confirms")
}

/// Fire a macOS notification (best effort, never fails the task).
pub fn notify(task_id: &str, task_type: &str, state_dir: &Path) {
    let hint = format!("fusion-link confirm {task_id} approve|deny");
    let script = format!(
        "display notification \"{} pending local approval. {}\" with title \"fusion-link\"",
        task_type, hint
    );
    match std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
    {
        Ok(_) => tracing::info!(task_id = %task_id, "confirmation notification posted"),
        Err(e) => tracing::warn!(task_id = %task_id, "notification failed (headless?): {e}"),
    }
    let _ = state_dir;
}

/// Wait for a decision file. Polls `confirms/<task_id>.decision`; resolves
/// approve|deny from its content. Session cancellation or timeout resolves
/// as a denial so a paused task can never hang the runner forever.
pub async fn wait_decision(
    state_dir: &Path,
    task_id: &str,
    timeout_secs: u64,
    cancel: &CancellationToken,
) -> Decision {
    let dir = confirm_dir(state_dir);
    let decision_path = dir.join(format!("{task_id}.decision"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs.max(1));
    loop {
        if let Ok(content) = std::fs::read_to_string(&decision_path) {
            let approved = content.trim().eq_ignore_ascii_case("approve");
            cleanup(state_dir, task_id);
            return Decision {
                approved,
                reason: if approved {
                    "approved locally".to_string()
                } else {
                    "denied by local user".to_string()
                },
            };
        }
        if cancel.is_cancelled() {
            cleanup(state_dir, task_id);
            return Decision {
                approved: false,
                reason: "device shutting down while awaiting confirmation".to_string(),
            };
        }
        if tokio::time::Instant::now() >= deadline {
            cleanup(state_dir, task_id);
            return Decision {
                approved: false,
                reason: format!("local confirmation timed out after {timeout_secs}s"),
            };
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            _ = cancel.cancelled() => {}
        }
    }
}

/// CLI escape hatch: write the decision file for a paused task.
pub fn write_decision(state_dir: &Path, task_id: &str, approve: bool) -> Result<(), String> {
    if !valid_task_id(task_id) {
        return Err(format!("invalid task id '{task_id}'"));
    }
    let dir = confirm_dir(state_dir);
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("confirm dir create failed {}: {e}", dir.display()))?;
    }
    let path = dir.join(format!("{task_id}.decision"));
    let content = if approve { "approve" } else { "deny" };
    std::fs::write(&path, format!("{content}\n"))
        .map_err(|e| format!("write {} failed: {e}", path.display()))?;
    tracing::info!(task_id = %task_id, "decision file written: {content}");
    Ok(())
}

/// Post the pending request file (audit trail + notification context).
pub fn post_request(state_dir: &Path, task_id: &str, task_type: &str) {
    let dir = confirm_dir(state_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("confirm dir create failed: {e}");
        return;
    }
    let path = dir.join(format!("{task_id}.request.json"));
    let body = serde_json::json!({"task_id": task_id, "task_type": task_type});
    if let Err(e) = std::fs::write(&path, body.to_string()) {
        tracing::warn!("confirm request write failed: {e}");
    }
}

fn cleanup(state_dir: &Path, task_id: &str) {
    let dir = confirm_dir(state_dir);
    for name in [
        format!("{task_id}.decision"),
        format!("{task_id}.request.json"),
    ] {
        let path = dir.join(name);
        if let Err(e) = std::fs::remove_file(&path)
            && path.exists()
        {
            tracing::warn!("confirm cleanup {} failed: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requires_confirm_matches_both_forms() {
        assert!(requires_confirm(&json!({"confirm": "local"})));
        assert!(requires_confirm(&json!({"confirm_required": true})));
        assert!(!requires_confirm(&json!({"confirm": "none"})));
        assert!(!requires_confirm(&json!({})));
    }

    #[tokio::test]
    async fn wait_decision_approve_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        post_request(state, "t-ok", "office.document");
        let cancel = CancellationToken::new();
        let w = tokio::spawn({
            let state = state.to_path_buf();
            let cancel = cancel.clone();
            async move { wait_decision(&state, "t-ok", 30, &cancel).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_decision(state, "t-ok", true).unwrap();
        let d = w.await.unwrap();
        assert!(d.approved);
        assert!(!confirm_dir(state).join("t-ok.decision").exists());
        assert!(!confirm_dir(state).join("t-ok.request.json").exists());
    }

    #[tokio::test]
    async fn wait_decision_deny_content() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        let cancel = CancellationToken::new();
        let w = tokio::spawn({
            let state = state.to_path_buf();
            let cancel = cancel.clone();
            async move { wait_decision(&state, "t-no", 30, &cancel).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_decision(state, "t-no", false).unwrap();
        let d = w.await.unwrap();
        assert!(!d.approved);
        assert!(d.reason.contains("denied"));
    }

    #[tokio::test]
    async fn wait_decision_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let d = wait_decision(dir.path(), "t-slow", 1, &cancel).await;
        assert!(!d.approved);
        assert!(d.reason.contains("timed out"));
    }

    #[tokio::test]
    async fn wait_decision_cancellation_denies() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let w = tokio::spawn({
            let cancel = cancel.clone();
            let path = dir.path().to_path_buf();
            async move { wait_decision(&path, "t-x", 60, &cancel).await }
        });
        cancel.cancel();
        let d = w.await.unwrap();
        assert!(!d.approved);
        assert!(d.reason.contains("shutting down"));
    }

    #[test]
    fn write_decision_rejects_bad_task_id_path() {
        let dir = tempfile::tempdir().unwrap();
        // path traversal guard: task ids come from the broker
        assert!(write_decision(dir.path(), "../evil", true).is_err());
        assert!(write_decision(dir.path(), "a/b", true).is_err());
        assert!(write_decision(dir.path(), "site_abc123", true).is_ok());
    }
}
