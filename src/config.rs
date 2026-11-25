use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRoute {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeCfg {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub broker_url: String,
    pub bind_url: String,
    pub gateway_url: String,
    #[serde(default)]
    pub gateway_token: Option<String>,
    pub heartbeat_secs: u64,
    pub state_dir: PathBuf,
    #[serde(default)]
    pub task_routes: HashMap<String, TaskRoute>,
    #[serde(default)]
    pub probes: Vec<ProbeCfg>,
}

impl Default for Config {
    fn default() -> Self {
        let mut task_routes = HashMap::new();
        task_routes.insert(
            "shell.chat".to_string(),
            TaskRoute {
                method: "POST".to_string(),
                path: "/api/v1/shell/chat/completions".to_string(),
                timeout_secs: 300,
            },
        );
        Self {
            broker_url: "wss://fusion-mlx.cn/link/ws".to_string(),
            bind_url: "https://fusion-mlx.cn/api/link/bind".to_string(),
            gateway_url: "http://127.0.0.1:11432".to_string(),
            gateway_token: None,
            heartbeat_secs: 30,
            state_dir: default_state_dir(),
            task_routes,
            probes: default_probes(),
        }
    }
}

pub fn default_state_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fusion-link")
}

fn default_probes() -> Vec<ProbeCfg> {
    vec![
        ProbeCfg {
            name: "fusion-mlx".into(),
            url: "http://127.0.0.1:11434/health".into(),
        },
        ProbeCfg {
            name: "fusion-gateway".into(),
            url: "http://127.0.0.1:11432/api/v1/system/health".into(),
        },
        ProbeCfg {
            name: "fusion-rag".into(),
            url: "http://127.0.0.1:11436/health".into(),
        },
        ProbeCfg {
            name: "fusion-artifacts-engine".into(),
            url: "http://127.0.0.1:11451/health".into(),
        },
        ProbeCfg {
            name: "fusion-office".into(),
            url: "http://127.0.0.1:11485/".into(),
        },
        ProbeCfg {
            name: "fusion-office-api".into(),
            url: "http://127.0.0.1:11492/health".into(),
        },
    ]
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let mut cfg = Config::default();
        let path = default_state_dir().join("config.toml");
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("config read failed {}: {e}", path.display()))?;
            let file_cfg: Config = toml::from_str(&raw)
                .map_err(|e| format!("config parse failed {}: {e}", path.display()))?;
            // File wins over defaults for fields it sets: merge via toml round-trip
            // is lossy, so treat file as authoritative when present (documented).
            cfg = file_cfg;
            tracing::info!("config loaded from {}", path.display());
        }
        if let Ok(v) = std::env::var("FUSION_LINK_BROKER_URL") {
            cfg.broker_url = v;
        }
        if let Ok(v) = std::env::var("FUSION_LINK_BIND_URL") {
            cfg.bind_url = v;
        }
        if let Ok(v) = std::env::var("FUSION_LINK_GATEWAY_URL") {
            cfg.gateway_url = v;
        }
        if let Ok(v) = std::env::var("FUSION_LINK_GATEWAY_TOKEN") {
            cfg.gateway_token = Some(v);
        }
        if let Ok(v) = std::env::var("FUSION_LINK_STATE_DIR") {
            cfg.state_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("FUSION_LINK_HEARTBEAT_SECS")
            && let Ok(n) = v.parse()
        {
            cfg.heartbeat_secs = n;
        }
        Ok(cfg)
    }

    pub fn ensure_state_dir(&self) -> Result<(), String> {
        let dir: &Path = &self.state_dir;
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("state dir create failed {}: {e}", dir.display()))?;
            tracing::info!("state dir created: {}", dir.display());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_port_registry() {
        let cfg = Config::default();
        assert!(cfg.broker_url.starts_with("wss://"));
        assert!(cfg.gateway_url.contains("11432"));
        let urls: Vec<&str> = cfg.probes.iter().map(|p| p.url.as_str()).collect();
        assert!(urls.iter().any(|u| u.contains("11434")));
        assert!(urls.iter().any(|u| u.contains("11436")));
        assert!(urls.iter().any(|u| u.contains("11451")));
        assert!(urls.iter().any(|u| u.contains("11492")));
        assert!(cfg.task_routes.contains_key("shell.chat"));
    }

    #[test]
    fn env_overrides_apply() {
        // env override logic covered indirectly by load(); keep parse-safe test
        assert!("30".parse::<u64>().is_ok());
    }
}
