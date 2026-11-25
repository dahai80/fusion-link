use crate::protocol::Envelope;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize, Default)]
struct OutboxState {
    next_seq: u64,
    pending: Vec<Envelope>,
}

#[derive(Debug)]
pub struct Outbox {
    path: PathBuf,
    state: OutboxState,
}

impl Outbox {
    pub fn load(path: PathBuf) -> Self {
        let state = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<OutboxState>(&raw).ok())
            .unwrap_or_default();
        if !state.pending.is_empty() {
            tracing::info!(
                "outbox restored: {} pending, next_seq={}",
                state.pending.len(),
                state.next_seq
            );
        }
        Self { path, state }
    }

    pub fn next_seq(&self) -> u64 {
        self.state.next_seq
    }

    pub fn pending(&self) -> &[Envelope] {
        &self.state.pending
    }

    pub fn pending_count(&self) -> usize {
        self.state.pending.len()
    }

    /// Assign seq to envelope and persist. Call before ws send so a crash
    /// between persist and send replays the message (at-least-once).
    pub fn push(&mut self, mut env: Envelope) -> Result<Envelope, String> {
        env.seq = self.state.next_seq;
        self.state.next_seq += 1;
        self.state.pending.push(env.clone());
        self.save()?;
        Ok(env)
    }

    /// Broker acknowledged delivery of all seq <= ack_seq.
    pub fn ack(&mut self, ack_seq: u64) -> Result<usize, String> {
        let before = self.state.pending.len();
        self.state.pending.retain(|e| e.seq > ack_seq);
        let dropped = before - self.state.pending.len();
        if dropped > 0 {
            tracing::debug!("outbox acked {} messages up to seq {}", dropped, ack_seq);
            self.save()?;
        }
        Ok(dropped)
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("outbox dir create failed: {e}"))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let raw =
            serde_json::to_vec(&self.state).map_err(|e| format!("outbox serialize failed: {e}"))?;
        std::fs::write(&tmp, &raw).map_err(|e| format!("outbox write failed: {e}"))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("outbox rename failed: {e}"))?;
        Ok(())
    }
}

pub fn outbox_path(state_dir: &Path) -> PathBuf {
    state_dir.join("outbox.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{MsgType, TaskEventPayload, TaskState};

    fn event_env(task_id: &str) -> Envelope {
        let payload = TaskEventPayload {
            task_id: task_id.into(),
            state: TaskState::Running,
            progress: None,
            detail: None,
        };
        Envelope::new(
            MsgType::TaskEvent,
            "dev-1",
            0,
            serde_json::to_value(&payload).unwrap(),
        )
    }

    #[test]
    fn push_assigns_monotonic_seq_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.json");
        let mut box1 = Outbox::load(path.clone());
        let e1 = box1.push(event_env("t1")).unwrap();
        let e2 = box1.push(event_env("t2")).unwrap();
        assert_eq!((e1.seq, e2.seq), (0, 1));

        let box2 = Outbox::load(path);
        assert_eq!(box2.pending_count(), 2);
        assert_eq!(box2.next_seq(), 2);
    }

    #[test]
    fn ack_drops_only_up_to_ack_seq() {
        let dir = tempfile::tempdir().unwrap();
        let mut outbox = Outbox::load(dir.path().join("outbox.json"));
        for _ in 0..3 {
            outbox.push(event_env("t")).unwrap();
        }
        let dropped = outbox.ack(1).unwrap();
        assert_eq!(dropped, 2);
        assert_eq!(outbox.pending_count(), 1);
        assert_eq!(outbox.pending()[0].seq, 2);
    }

    #[test]
    fn ack_without_progress_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mut outbox = Outbox::load(dir.path().join("outbox.json"));
        outbox.push(event_env("t")).unwrap();
        assert_eq!(outbox.ack(99).unwrap(), 1);
        assert_eq!(outbox.pending_count(), 0);
        assert_eq!(outbox.ack(99).unwrap(), 0);
    }
}
